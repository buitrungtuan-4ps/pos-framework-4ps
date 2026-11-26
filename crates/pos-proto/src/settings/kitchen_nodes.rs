// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The register's entry for how a guest's QR order reaches the kitchen
//! ([ADR-0247](../../../../docs/adr/0247-a-guest-or-vendor-order-reaches-the-kitchen-as-the-store-chooses.md)),
//! apart from `nodes.rs`, which is at its size limit. A vendor's own choice is a field of its
//! delivery connection, not a setting.

use crate::kitchen_release::KitchenRelease;
use crate::qr::{LAST_MINUTE_OF_DAY, PublishedQr};
use crate::wire_enum::WireEnum;

use super::{NEXT_RELEASE, STORE_WIDE, Setting, SettingShape, SettingUnit, choices};

/// The `qr` node's `kitchen_release` (ADR-0247 decision 2), and the hours its automatic send keeps
/// (ADR-0255).
pub(super) fn kitchen_release_settings() -> Vec<Setting> {
    let mut settings = vec![Setting {
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
    }];
    settings.extend(auto_fire_hours_settings());
    settings
}

/// One end of the window `AUTO_ACCEPT_AND_FIRE` keeps (ADR-0255 decision 1).
fn auto_fire_minute(field: &'static str, summary: &'static str) -> Setting {
    Setting {
        node: PublishedQr::NODE,
        field,
        shape: SettingShape::Int {
            min: 0,
            max: i64::from(LAST_MINUTE_OF_DAY),
            unit: SettingUnit::Minutes,
            default: 0,
            preset: None,
        },
        scopes: STORE_WIDE,
        since: NEXT_RELEASE,
        confirm: None,
        summary,
    }
}

/// The `qr` node's hours for `AUTO_ACCEPT_AND_FIRE`, and the mode outside them (ADR-0255).
fn auto_fire_hours_settings() -> Vec<Setting> {
    vec![
        auto_fire_minute(
            "auto_fire_from_minute",
            "The minute after the store's local midnight from which a QR order is sent with no \
             tap, under `KITCHEN_RELEASE_AUTO_ACCEPT_AND_FIRE`: 600 is 10:00. The window runs to \
             `auto_fire_until_minute`, wraps past midnight when it starts later than it ends, and \
             is always open when the two are equal, as both are by default.",
        ),
        auto_fire_minute(
            "auto_fire_until_minute",
            "The minute after the store's local midnight at which a QR order stops being sent \
             with no tap: 840 is 14:00. Outside the window an order arrives as \
             `kitchen_release_outside_hours` says.",
        ),
        Setting {
            node: PublishedQr::NODE,
            field: "kitchen_release_outside_hours",
            shape: SettingShape::Choice {
                values: vec![
                    KitchenRelease::ConfirmFires.as_wire(),
                    KitchenRelease::ConfirmThenSend.as_wire(),
                ],
                default: KitchenRelease::ConfirmThenSend.as_wire(),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "How a QR order that arrives outside the automatic send's hours reaches the \
                      kitchen, under `KITCHEN_RELEASE_AUTO_ACCEPT_AND_FIRE`. \
                      `KITCHEN_RELEASE_CONFIRM_THEN_SEND`, the default, waits for staff to accept \
                      it and then send it; `_CONFIRM_FIRES` sends it in the tap that accepts it.",
        },
    ]
}
