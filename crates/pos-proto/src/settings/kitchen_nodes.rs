// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The register's entry for how a guest's QR order reaches the kitchen
//! ([ADR-0247](../../../../docs/adr/0247-a-guest-or-vendor-order-reaches-the-kitchen-as-the-store-chooses.md)),
//! apart from `nodes.rs`, which is at its size limit. A vendor's own choice is a field of its
//! delivery connection, not a setting.

use crate::kitchen_release::KitchenRelease;
use crate::qr::PublishedQr;
use crate::wire_enum::WireEnum;

use super::{NEXT_RELEASE, STORE_WIDE, Setting, SettingShape, choices};

/// The `qr` node's `kitchen_release` (ADR-0247 decision 2).
pub(super) fn kitchen_release_settings() -> Vec<Setting> {
    vec![Setting {
        node: PublishedQr::NODE,
        field: "kitchen_release",
        shape: SettingShape::Choice {
            values: choices::<KitchenRelease>(),
            default: PublishedQr::default().kitchen_release().as_wire(),
            // The safe choice: nothing reaches the kitchen unseen unless the store says so.
            preset: None,
        },
        scopes: STORE_WIDE,
        since: NEXT_RELEASE,
        confirm: None,
        summary: "How a guest's QR order reaches the kitchen. `KITCHEN_RELEASE_CONFIRM_THEN_SEND`, \
                  the default, takes two steps: staff accept the order, then send it as any \
                  order. `_CONFIRM_FIRES` sends it in the same tap that accepts it. \
                  `_AUTO_ACCEPT_AND_FIRE` accepts it on arrival and sends it with no tap, so a \
                  guest's order reaches the kitchen unseen; the table token, the rate limit and \
                  the hours still apply. Where `staff_confirmation_required` is off, nothing waits \
                  and staff send the order.",
    }]
}
