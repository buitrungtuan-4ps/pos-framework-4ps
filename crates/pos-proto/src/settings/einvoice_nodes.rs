// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The register's entries for the `einvoice` node
//! ([ADR-0229](../../../../docs/adr/0229-the-e-invoice-issue-moment-is-a-schedule.md)), apart from
//! `nodes.rs`, which is at its size limit.

use crate::einvoice::{
    self, EInvoiceIssueMoment, EInvoiceNumberSource, EInvoiceSymbolAllocation, PublishedEInvoice,
};
use crate::wire_enum::WireEnum;

use super::{NEXT_RELEASE, STORE_WIDE, Setting, SettingShape, SettingUnit, choices};

/// The settings on the `einvoice` node (ADR-0229 decision 1, ADR-0240 decision 4). The node's
/// `symbol` is the cloud's to give each issuing point and is not a setting.
pub(super) fn einvoice_settings() -> Vec<Setting> {
    vec![
        Setting {
            node: PublishedEInvoice::NODE,
            field: "issue_moment",
            shape: SettingShape::Choice {
                values: choices::<EInvoiceIssueMoment>(),
                default: PublishedEInvoice::default().issue_moment().as_wire(),
                // Seeded by the country pack (ADR-0183), and no template sets it.
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "When a settled bill issues its legal e-invoice, numbered under the symbol \
                      the cloud gives the store and submitted by the cloud. \
                      `EINVOICE_ISSUE_MOMENT_OFF`, the default, issues none; `_IMMEDIATE`, which \
                      the Vietnamese pack seeds, issues with the settle; `_SCHEDULED_PER_ORDER` \
                      `einvoice.delay_minutes` after it; `_BATCH_WINDOW` together at the daily \
                      `einvoice.batch_minute`; `_ON_REQUEST` only for a bill whose guest gave buyer \
                      details. No invoice is issued later than the store's country allows. A store \
                      that must issue and has no symbol refuses to settle.",
        },
        Setting {
            node: PublishedEInvoice::NODE,
            field: "delay_minutes",
            shape: SettingShape::Int {
                min: *einvoice::DELAY_MINUTES.start(),
                max: *einvoice::DELAY_MINUTES.end(),
                unit: SettingUnit::Minutes,
                default: 0,
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "How many minutes after its settle a bill's e-invoice is issued under \
                      `EINVOICE_ISSUE_MOMENT_SCHEDULED_PER_ORDER`. `0`, the default, issues at \
                      the next tick. A delay that could pass the country's legal limit is refused.",
        },
        Setting {
            node: PublishedEInvoice::NODE,
            field: "batch_minute",
            shape: SettingShape::Int {
                min: *einvoice::BATCH_MINUTE.start(),
                max: *einvoice::BATCH_MINUTE.end(),
                unit: SettingUnit::Minutes,
                default: einvoice::DEFAULT_BATCH_MINUTE,
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "The minute of the store's day, counted from local midnight, at which \
                      `EINVOICE_ISSUE_MOMENT_BATCH_WINDOW` issues every invoice waiting. `1320`, \
                      the default, is 22:00. A bill settled after it waits for the next day's \
                      batch, unless the country's legal limit comes first.",
        },
        Setting {
            node: PublishedEInvoice::NODE,
            field: "symbol_allocation",
            shape: SettingShape::Choice {
                values: choices::<EInvoiceSymbolAllocation>(),
                default: PublishedEInvoice::default().symbol_allocation().as_wire(),
                // Seeded by the country pack (ADR-0183), and no template sets it.
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "Whether the cloud allocates the store's e-invoice symbol by itself. \
                      `EINVOICE_SYMBOL_ALLOCATION_MANUAL`, the default, leaves it to HQ, by hand \
                      or with the console's allocate button; `_AUTOMATIC`, which the Vietnamese \
                      pack seeds, allocates one when a box is activated for a store with none and \
                      with each lease bump. The cloud reads it; the edge does not.",
        },
        number_source_setting(),
    ]
}

/// `einvoice.number_source`: who numbers the store's invoices
/// ([ADR-0251](../../../../docs/adr/0251-a-vendor-numbered-invoice-waits-for-its-number-and-the-store-learns-it.md)).
/// `HYBRID` is offered only once its offline symbol is built (decision 8).
pub(super) fn number_source_setting() -> Setting {
    Setting {
        node: PublishedEInvoice::NODE,
        field: "number_source",
        shape: SettingShape::Choice {
            values: vec![
                EInvoiceNumberSource::Pos.as_wire(),
                EInvoiceNumberSource::Vendor.as_wire(),
            ],
            default: PublishedEInvoice::default().number_source().as_wire(),
            // Which source a seller may use is to be confirmed by legal and the vendor, so no
            // template chooses for the tenant.
            preset: None,
        },
        scopes: STORE_WIDE,
        since: NEXT_RELEASE,
        confirm: None,
        summary: "Who numbers the store's e-invoices. `EINVOICE_NUMBER_SOURCE_POS`, the default, \
                  numbers each at issue from the store's own symbol, offline too; `_VENDOR` issues \
                  it at the same moment \"number pending\", the cloud asks the vendor for the \
                  number, and the store learns it at its next config pull, after which a copy of \
                  the receipt prints it. Numbering stays gapless either way: a store switched \
                  back to the POS keeps issuing \"number pending\" until no invoice waits. \
                  Whether a seller may let the vendor number is to be confirmed by legal and the \
                  vendor.",
    }
}

/// The `billing` node's card setting (ADR-0243), apart from `nodes.rs`, which is at its size limit.
pub(super) fn card_confirm_settings() -> Vec<Setting> {
    vec![Setting {
        node: crate::billing::PublishedBilling::NODE,
        field: "card_confirm_approval",
        shape: SettingShape::Choice {
            values: choices::<crate::billing::CardConfirmApproval>(),
            default: crate::billing::PublishedBilling::default()
                .card_confirm_approval()
                .as_wire(),
            // Money integrity: no pack and no template changes it (ADR-0243 decision 1).
            preset: None,
        },
        scopes: STORE_WIDE,
        since: NEXT_RELEASE,
        confirm: None,
        summary: "Who must agree before a card the terminal left unknown is confirmed as paid by \
                  hand. `CARD_CONFIRM_APPROVAL_MANAGER`, the default, needs a manager's badge and \
                  PIN, as a void does; `_NONE` lets the person who may take payment confirm it \
                  alone. Looking the card up or cancelling it on the terminal needs neither.",
    }]
}
