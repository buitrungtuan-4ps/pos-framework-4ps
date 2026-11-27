// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The register's entries for correcting an invoiced bill
//! ([ADR-0245](../../../../docs/adr/0245-an-invoiced-bill-is-corrected-by-the-route-the-store-chooses.md)),
//! apart from `einvoice_nodes.rs`.
//!
//! Every statement of law these settings rest on is to be confirmed by legal or the accountant.

use crate::einvoice::{self, EInvoiceKind, InvoicedCorrection, PublishedEInvoice};
use crate::wire_enum::WireEnum;

use super::{NEXT_RELEASE, STORE_WIDE, Setting, SettingShape, SettingUnit};

/// The settings on the `einvoice` node that say how an invoiced bill is corrected (ADR-0245).
pub(super) fn correction_settings() -> Vec<Setting> {
    vec![
        route_setting(),
        Setting {
            node: PublishedEInvoice::NODE,
            field: "manager_correction_limit",
            shape: SettingShape::Int {
                min: *einvoice::MANAGER_CORRECTION_LIMIT.start(),
                max: *einvoice::MANAGER_CORRECTION_LIMIT.end(),
                unit: SettingUnit::MinorUnits,
                default: 0,
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "The largest bill total, in the currency's minor unit, a manager may void at \
                      the till when its e-invoice was issued, under \
                      `INVOICED_CORRECTION_MANAGER_UP_TO_LIMIT`. `0`, the default, allows none.",
        },
        Setting {
            node: PublishedEInvoice::NODE,
            field: "cancel_window_minutes",
            shape: SettingShape::Int {
                min: *einvoice::CANCEL_WINDOW_MINUTES.start(),
                max: *einvoice::CANCEL_WINDOW_MINUTES.end(),
                unit: SettingUnit::Minutes,
                default: 0,
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "How many minutes after its e-invoice was issued a manager may still void a \
                      bill at the till, under `INVOICED_CORRECTION_CANCEL_WINDOW`. `0`, the \
                      default, allows none. Whether an invoice not yet submitted may be cancelled \
                      rather than corrected is to be confirmed by legal or the accountant; until \
                      then the cloud corrects it.",
        },
        Setting {
            node: PublishedEInvoice::NODE,
            field: "correction_kind",
            shape: SettingShape::Choice {
                values: vec![
                    EInvoiceKind::Adjustment.as_wire(),
                    EInvoiceKind::Replacement.as_wire(),
                ],
                default: PublishedEInvoice::default().correction_kind().as_wire(),
                // Which errors need which kind is to be confirmed by legal or the accountant, so no
                // template chooses for the tenant.
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "What the cloud issues when a bill with an issued e-invoice is voided at the \
                  till. `EINVOICE_KIND_ADJUSTMENT`, the default, takes the invoice's standing \
                  total back to zero; `EINVOICE_KIND_REPLACEMENT` replaces it with an invoice of \
                  zero. Either is numbered under the tenant's correction symbol and never takes \
                  an invoice below zero. Which errors need which kind is to be confirmed by legal \
                  or the accountant. The edge writes the kind it runs on each void of an invoiced \
                  bill, and the cloud corrects with that kind (ADR-0256), so a later change \
                  does not reach an earlier void.",
        },
        Setting {
            node: PublishedEInvoice::NODE,
            field: "correction_reason_required",
            shape: SettingShape::Bool {
                default: PublishedEInvoice::default().correction_reason_required(),
                preset: None,
            },
            scopes: STORE_WIDE,
            since: NEXT_RELEASE,
            confirm: None,
            summary: "Whether a manager's request to correct a bill with an issued e-invoice must \
                      name a reason from the store's `VOID_BILL` reason codes. On, the default; \
                      off lets a request go without one. Whether a correction needs a reason is to \
                      be confirmed by legal or the accountant. The edge reads it.",
        },
    ]
}

/// `einvoice.invoiced_correction`: which route a bill with an issued e-invoice takes (ADR-0245).
fn route_setting() -> Setting {
    Setting {
        node: PublishedEInvoice::NODE,
        field: "invoiced_correction",
        shape: SettingShape::Choice {
            // `REQUEST` is offered now its console queue, the till's form and its honouring
            // of an approval are built (ADR-0250, Consequences).
            values: vec![
                InvoicedCorrection::HqOnly.as_wire(),
                InvoicedCorrection::Request.as_wire(),
                InvoicedCorrection::ManagerUpToLimit.as_wire(),
                InvoicedCorrection::CancelWindow.as_wire(),
            ],
            default: PublishedEInvoice::default().invoiced_correction().as_wire(),
            preset: None,
        },
        scopes: STORE_WIDE,
        since: NEXT_RELEASE,
        confirm: None,
        summary: "How a bill with an issued e-invoice is voided. \
                  `INVOICED_CORRECTION_HQ_ONLY`, the default, refuses at the till and HQ \
                  corrects it in the console; `_REQUEST` lets a manager ask HQ from the till \
                  and void the bill there once HQ approves it in the console's queue; \
                  `_MANAGER_UP_TO_LIMIT` lets a manager void it at \
                  the till when its total is at most `einvoice.manager_correction_limit`; \
                  `_CANCEL_WINDOW` while its invoice is younger than \
                  `einvoice.cancel_window_minutes`. Above the limit or after the window, the \
                  manager may ask HQ as under `_REQUEST`. Anything else is refused at the till. The \
                  cloud then corrects the invoice as `einvoice.correction_kind` says. Every \
                  legal point is to be confirmed by legal or the accountant.",
    }
}
