// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The entries of the component catalogue. A new component is a release: its implementation in the
//! till first, then its entry here (ADR-0190 decision 2). The screens of the default package add the
//! components they are drawn from, each with the part of the context it reads (ADR-0203).

use super::PropertyKind::{Action, Token, TranslationKey};
use super::{Component, Property};
use crate::devices::DeviceClass;
use crate::settings::NEXT_RELEASE;

/// Every device class a till runs on.
const EVERY_CLASS: &[DeviceClass] = &[
    DeviceClass::Terminal,
    DeviceClass::Tablet,
    DeviceClass::Phone,
    DeviceClass::KitchenDisplay,
    DeviceClass::Kiosk,
];

/// What every entry starts from: reads nothing, takes nothing, holds nothing, supplies nothing,
/// drawn on every class, no fallback, introduced in the next release.
const BASE: Component = Component {
    component_id: "",
    reads: "",
    properties: &[],
    holds_children: false,
    supplies: &[],
    device_classes: EVERY_CLASS,
    fallback_component: None,
    since: NEXT_RELEASE,
    summary: "",
};

/// What every property starts from: optional, no tokens, no bounds.
const PROPERTY: Property = Property {
    name: "",
    kind: Token,
    required: false,
    tokens: &[],
    bounds: (0, 0),
};

/// The ids the screen itself is for, which any component that binds an act supplies.
const SCREEN_SUBJECT: &[&str] = &["order_id", "table_id"];

/// Every component, in the order the console lists them.
pub static CATALOGUE: &[Component] = &[
    Component {
        component_id: "stack",
        properties: &[Property {
            name: "direction",
            tokens: &["vertical", "horizontal"],
            ..PROPERTY
        }],
        holds_children: true,
        summary: "Its children one after another, down the screen or across it.",
        ..BASE
    },
    Component {
        component_id: "text",
        properties: &[
            Property {
                name: "text_key",
                kind: TranslationKey,
                required: true,
                ..PROPERTY
            },
            Property {
                name: "variant",
                tokens: &["title", "body", "muted"],
                ..PROPERTY
            },
        ],
        summary: "One line of text from the translation grid.",
        ..BASE
    },
    Component {
        component_id: "action_button",
        reads: "subject",
        properties: &[
            Property {
                name: "label_key",
                kind: TranslationKey,
                required: true,
                ..PROPERTY
            },
            Property {
                name: "action",
                kind: Action,
                required: true,
                ..PROPERTY
            },
            Property {
                name: "variant",
                tokens: &["primary", "secondary", "danger"],
                ..PROPERTY
            },
        ],
        supplies: SCREEN_SUBJECT,
        summary: "A button that does one act on the order or table the screen is for.",
        ..BASE
    },
    Component {
        component_id: "screen_frame",
        holds_children: true,
        summary: "A whole screen's frame, its children inside the screen's margin.",
        ..BASE
    },
    Component {
        component_id: "page_header",
        properties: &[Property {
            name: "title_key",
            kind: TranslationKey,
            required: true,
            ..PROPERTY
        }],
        summary: "A screen's heading.",
        ..BASE
    },
    Component {
        component_id: "floor_alerts",
        reads: "floor",
        summary: "Why the floor's last tap was refused, until the operator dismisses it.",
        ..BASE
    },
    Component {
        component_id: "floor_plan",
        reads: "floor",
        supplies: &["table_id"],
        summary: "Every table of the floor by area, placed as the store drew it: a tap seats a free \
                  table and opens it, opens a used one, or clears one that needs cleaning.",
        ..BASE
    },
    Component {
        component_id: "order_layout",
        holds_children: true,
        summary: "The order screen's two halves: its column and its menu, side by side on a terminal, the menu as the screen on a tablet.",
        ..BASE
    },
    Component {
        component_id: "order_column",
        holds_children: true,
        summary: "The order's own column: its header and its bill; on a tablet its body is a panel over the menu's foot.",
        ..BASE
    },
    Component {
        component_id: "order_header",
        reads: "order",
        summary: "The order's heading: the way back, the table or counter order, its state, moving the table and printing its pre-bill.",
        ..BASE
    },
    Component {
        component_id: "order_notices",
        reads: "order",
        summary: "What the last pre-bill print and table move said.",
        ..BASE
    },
    Component {
        component_id: "order_move",
        reads: "order",
        summary: "The free tables the order's guests can move to, while the operator is moving them.",
        ..BASE
    },
    Component {
        component_id: "order_bill",
        reads: "order",
        summary: "The order's lines, choices, seats, fees and totals, and the acts that send it and take payment.",
        ..BASE
    },
    Component {
        component_id: "order_menu",
        holds_children: true,
        summary: "The menu's side of the order screen.",
        ..BASE
    },
    Component {
        component_id: "menu_toolbar",
        reads: "menu",
        summary: "The menu's heading, marking a dish sold out, and why the menu sells nothing while the bill is open.",
        ..BASE
    },
    Component {
        component_id: "menu_fields",
        reads: "menu",
        summary: "The menu's search and the note that rides with the next dish.",
        ..BASE
    },
    Component {
        component_id: "menu_buttons",
        reads: "menu",
        summary: "The menu's buttons as the store arranged them, or what the search found.",
        ..BASE
    },
];
