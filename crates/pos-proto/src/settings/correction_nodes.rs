// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The register's entries for correcting an invoiced bill
//! ([ADR-0245](../../../../docs/adr/0245-an-invoiced-bill-is-corrected-by-the-route-the-store-chooses.md)),
//! apart from `einvoice_nodes.rs`.
//!
//! Every statement of law these settings rest on is to be confirmed by legal or the accountant.

use crate::einvoice::{EInvoiceKind, PublishedEInvoice};
use crate::wire_enum::WireEnum;

use super::{NEXT_RELEASE, STORE_WIDE, Setting, SettingShape};

/// The settings on the `einvoice` node that say how an invoiced bill is corrected (ADR-0245).
pub(super) fn correction_settings() -> Vec<Setting> {
    vec![Setting {
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
    }]
}
