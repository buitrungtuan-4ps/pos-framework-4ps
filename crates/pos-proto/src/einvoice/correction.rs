// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A store manager's request to correct a bill's issued invoice, and HQ's decision on it
//! ([ADR-0245](../../../../docs/adr/0245-an-invoiced-bill-is-corrected-by-the-route-the-store-chooses.md)
//! route b, [ADR-0250](../../../../docs/adr/0250-a-store-learns-a-correction-decision-on-its-einvoice-node.md)).
//!
//! No personal data: the requester is the event envelope's employee, and the approver is named by
//! the console user's id alone. Every statement of law behind the route is to be confirmed by
//! legal or the accountant.

use serde::{Deserialize, Deserializer, Serialize};

use super::EInvoiceKind;
use crate::ids::{BillId, ReasonCodeId};
use crate::money::Money;
use crate::time::Timestamp;
use crate::ulid::Ulid;
use crate::wire_enum;
use crate::wire_enum::Open;

/// The most decisions a store's `einvoice` node carries (ADR-0250 decision 1).
pub const CORRECTION_DECISIONS_MAX: usize = 200;

wire_enum! {
    /// What HQ or the accountant decided on a correction request (ADR-0250 decision 2).
    CorrectionDecision, prefix = "CORRECTION_DECISION";
    /// The manager may void the bill at the till, once; the cloud then corrects its invoice.
    Approved = "APPROVED",
    /// The void stays refused at the till.
    Declined = "DECLINED",
}

/// What `fiscal.correction.requested` carries: the bill, the amount, the kind and the reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct CorrectionRequest {
    /// The settled bill whose invoice is to be corrected.
    pub bill_id: BillId,
    /// The amount to take back: the bill's total, since a paid bill is voided whole (ADR-0184).
    pub amount: Money,
    /// The kind the cloud will issue, from `einvoice.correction_kind`.
    pub kind: Open<EInvoiceKind>,
    /// Why, from the store's `VOID_BILL` reason codes. Absent only where
    /// `einvoice.correction_reason_required` is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason_code_id: Option<ReasonCodeId>,
}

/// One decision as the cloud writes it on the store's `einvoice` node, and as
/// `fiscal.correction.decided` records it at the store (ADR-0250 decision 2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct DecidedCorrection {
    /// The bill the request named.
    pub bill_id: BillId,
    /// Approved or declined. Read it through [`Self::approved`].
    pub decision: Open<CorrectionDecision>,
    /// The console user who decided, by id alone.
    pub decided_by_user_id: Ulid,
    /// When it was decided.
    pub decided_time: Timestamp,
}

// The personal-data barrier for each part, field by field, and their fields in the snapshot.
crate::envelope::payload_part!(CorrectionRequest {
    bill_id,
    amount,
    kind,
    reason_code_id,
});
crate::envelope::payload_part!(DecidedCorrection {
    bill_id,
    decision,
    decided_by_user_id,
    decided_time,
});

impl DecidedCorrection {
    /// Whether the bill may be voided at the till: only a known `APPROVED`.
    #[must_use]
    pub fn approved(&self) -> bool {
        self.decision.known() == CorrectionDecision::Approved
    }
}

/// Reads the node's `correction_decisions` leniently: an entry this release cannot read is
/// dropped rather than refusing the whole node, and at most [`CORRECTION_DECISIONS_MAX`] are kept,
/// the newest last.
pub(super) fn decisions<'de, D>(deserializer: D) -> Result<Vec<DecidedCorrection>, D::Error>
where
    D: Deserializer<'de>,
{
    let raw = serde_json::Value::deserialize(deserializer)?;
    let mut read: Vec<DecidedCorrection> = raw
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| DecidedCorrection::deserialize(entry).ok())
        .collect();
    let excess = read.len().saturating_sub(CORRECTION_DECISIONS_MAX);
    read.drain(..excess);
    Ok(read)
}
