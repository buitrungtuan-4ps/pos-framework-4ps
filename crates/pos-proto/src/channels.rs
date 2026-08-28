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

use crate::enums::{PaymentMethod, SalesChannel, VendorAvailability};
use crate::ids::MenuItemId;
use crate::text::DisplayName;
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

/// One store's policy toward a single delivery marketplace, in wire form ([ADR-0080](../../../docs/adr/0080-channels-and-payments.md), M7).
///
/// A lightweight authoring shape only: which vendor, whether it is on, its availability (open/busy/
/// closed, mirroring the `DeliveryVendor` busy-mode), the prep time authored for the busy case, and the
/// menu items suppressed (86'd) on that vendor. The live loop that pushes this to a marketplace is the
/// flagged follow-up; publishing the policy is this track.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedVendorPolicy {
    /// The vendor's operator-facing name (e.g. the marketplace brand). Reference/display only.
    pub vendor: DisplayName,
    /// Whether the store currently offers orders to this vendor at all.
    #[serde(default)]
    pub enabled: bool,
    /// The store's availability to this vendor. Wrapped in [`Open`] for forward-compatibility.
    #[serde(default)]
    pub availability: Open<VendorAvailability>,
    /// The prep time (minutes) authored for the busy case — how long the vendor should quote while the
    /// store is throttling. `0` means "use the vendor's default".
    #[serde(default)]
    pub prep_minutes: u16,
    /// Menu items suppressed (86'd) on this vendor specifically, independent of stock-driven auto-86.
    #[serde(default)]
    pub suppressed_items: Vec<MenuItemId>,
}

/// The `vendors` config node: a store's per-marketplace policies.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct PublishedVendorPolicies {
    #[serde(default)]
    policies: Vec<PublishedVendorPolicy>,
}

impl PublishedVendorPolicies {
    /// A node from its policies.
    #[must_use]
    pub fn new(policies: Vec<PublishedVendorPolicy>) -> Self {
        Self { policies }
    }

    /// Every vendor policy, in the order the node lists them.
    #[must_use]
    pub fn policies(&self) -> &[PublishedVendorPolicy] {
        &self.policies
    }

    /// Whether the node carries no policy at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.policies.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        PublishedChannels, PublishedTender, PublishedVendorPolicies, PublishedVendorPolicy,
    };
    use crate::enums::{PaymentMethod, SalesChannel, VendorAvailability};
    use crate::text::DisplayName;
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
    fn a_vendors_node_round_trips_and_defaults_empty() {
        use crate::ids::MenuItemId;
        use crate::ulid::Ulid;
        let node = PublishedVendorPolicies::new(vec![PublishedVendorPolicy {
            vendor: DisplayName::new("GrabFood"),
            enabled: true,
            availability: Open::from_known(VendorAvailability::Busy),
            prep_minutes: 25,
            suppressed_items: vec![MenuItemId::new(Ulid::from_u128(9))],
        }]);
        let json = serde_json::to_string(&node).expect("serialize");
        let back: PublishedVendorPolicies = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(node, back);
        assert_eq!(back.policies().len(), 1);
        assert!(PublishedVendorPolicies::default().is_empty());
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
