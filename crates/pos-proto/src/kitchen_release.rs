// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! How a guest's QR order or a vendor's delivery order reaches the kitchen
//! ([ADR-0247](../../../docs/adr/0247-a-guest-or-vendor-order-reaches-the-kitchen-as-the-store-chooses.md)):
//! the `qr` node's `kitchen_release`, and each vendor's in the `vendors` node.

use serde::Deserialize;
use serde_json::Value;

use crate::wire_enum;
use crate::wire_enum::Open;

wire_enum! {
    /// How an order from a guest or a vendor reaches the kitchen (ADR-0247 decision 1).
    KitchenRelease, prefix = "KITCHEN_RELEASE";
    /// No tap: the order is accepted on arrival and its lines fired with it.
    AutoAcceptAndFire = "AUTO_ACCEPT_AND_FIRE",
    /// One tap: confirming the order accepts it and fires its unsent lines with it.
    ConfirmFires = "CONFIRM_FIRES",
    /// Two steps: confirming accepts the order, and its lines are then sent as any order's are.
    /// The default, and the behaviour before the setting.
    ConfirmThenSend = "CONFIRM_THEN_SEND",
}

impl KitchenRelease {
    /// A node's value as the store runs it: [`Self::ConfirmThenSend`] for an absent value and for
    /// one this release does not know, the safe reading that sends nothing unseen.
    #[must_use]
    pub fn read(value: &Open<Self>) -> Self {
        match value.known() {
            Self::AutoAcceptAndFire => Self::AutoAcceptAndFire,
            Self::ConfirmFires => Self::ConfirmFires,
            Self::Unspecified | Self::ConfirmThenSend => Self::ConfirmThenSend,
        }
    }
}

/// A release as a node carries it: a token, kept as it came where this release does not know it,
/// and nothing for a value that is not a token, so the field reads as its default and never takes
/// its node down with it.
pub(crate) fn token<'de, D>(deserializer: D) -> Result<Open<KitchenRelease>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Value::deserialize(deserializer)?
        .as_str()
        .map_or_else(Open::default, Open::parse))
}

#[cfg(test)]
mod tests {
    use super::KitchenRelease;
    use crate::wire_enum::Open;

    #[test]
    fn an_absent_or_unknown_release_sends_nothing_unseen() {
        assert_eq!(
            KitchenRelease::read(&Open::default()),
            KitchenRelease::ConfirmThenSend
        );
        assert_eq!(
            KitchenRelease::read(&Open::parse("KITCHEN_RELEASE_TELEPATHY")),
            KitchenRelease::ConfirmThenSend
        );
        assert_eq!(
            KitchenRelease::read(&Open::from_known(KitchenRelease::ConfirmFires)),
            KitchenRelease::ConfirmFires
        );
    }
}
