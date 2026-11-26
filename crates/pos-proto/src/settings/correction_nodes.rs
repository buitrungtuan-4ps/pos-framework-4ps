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
        Setting {
            node: PublishedEInvoice::NODE,
            field: "invoiced_correction",
            shape: SettingShape::Choice {
                // `REQUEST` is offered once its console queue exists (ADR-0245 decision 1).
                values: vec![
                    InvoicedCorrection::HqOnly.as_wire(),
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
                      corrects it in the console; `_MANAGER_UP_TO_LIMIT` lets a manager void it at \
                      the till when its total is at most `einvoice.manager_correction_limit`; \
                      `_CANCEL_WINDOW` while its invoice is younger than \
                      `einvoice.cancel_window_minutes`. Anything else is refused at the till. The \
                      cloud then corrects the invoice as `einvoice.correction_kind` says. Every \
                      legal point is to be confirmed by legal or the accountant.",
        },
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
                  or the accountant. The cloud reads it; the edge does not.",
        },
    ]
}
