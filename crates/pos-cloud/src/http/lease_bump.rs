// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The two parts of [`admin_bump_lease`](super::admin_bump_lease) that read only its request and
//! its result: the placement a bump asks for, and what the audit trail records of it
//! ([ADR-0110](../../../../docs/adr/0110-edge-placement-is-a-deployment-axis.md)).

use axum::response::Response;

use pos_core::lease::LeaseGeneration;
use pos_proto::ErrorStatus;
use pos_proto::enums::EdgePlacement;
use pos_proto::wire_enum::WireEnum as _;

use super::api_error_with_details;
use crate::lease::{LeaseBump, Region};

/// The placement a bump moves the store to, or `None` for a swap in place; refused when the token is
/// not a placement, `EDGE_PLACEMENT_UNSPECIFIED` included.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
pub(super) fn requested_placement(
    edge_placement: Option<&str>,
) -> Result<Option<EdgePlacement>, Response> {
    let requested_placement = match edge_placement {
        None => None,
        Some(token) => match EdgePlacement::from_wire(token) {
            Some(EdgePlacement::Unspecified) | None => {
                return Err(api_error_with_details(
                    ErrorStatus::InvalidArgument,
                    "edge_placement must be one of EDGE_PLACEMENT_IN_STORE, \
                     EDGE_PLACEMENT_HOSTED_BY_OPERATOR, EDGE_PLACEMENT_HOSTED_BY_PLATFORM — \
                     omit the field to keep the store where it is",
                    &[("edge_placement", "UNKNOWN_VALUE")],
                ));
            }
            Some(placement) => Some(placement),
        },
    };
    Ok(requested_placement)
}

/// What the audit trail records of a bump: the generation issued, where the store now runs and rests,
/// and what the bump abandoned and set as the receipt floor.
pub(super) fn bump_audit_detail(
    bump: &LeaseBump,
    requested_placement: Option<EdgePlacement>,
    acknowledged: Option<LeaseGeneration>,
    receipt_floor: Option<u64>,
) -> serde_json::Value {
    serde_json::json!({
        "generation": bump.generation.value(),
        "edge_placement": bump.edge_placement.as_wire(),
        "edge_placement_moved": requested_placement.is_some(),
        // Where the store's data now rests, and the place a person would recognise
        // (ADR-0114). Both read from what the write returned rather than echoed from the
        // request, so a swap in place records the region the store *has* rather than the
        // nothing this body said. A country code is business metadata and goes in whole;
        // the label is free text and goes in whole too, because an entry whose payload is
        // redacted does not record what was chosen, which is the only thing this entry
        // exists to record (ADR-0069's redaction rule is about personal data, and the
        // console's field asks for a place).
        "region_country": bump.region.as_ref().map(Region::country_str),
        "region_label": bump.region.as_ref().map(Region::label),
        // The generation whose events this bump knowingly abandoned, or `null`. It has to
        // be here because it is nowhere else: an acknowledged bump does not *clear*
        // `superseded_generation`, it rolls it forward to the machine it is displacing now.
        // The trail is the only record that an earlier machine's sales were written off,
        // and by whom.
        "abandoned_generation": acknowledged.map(LeaseGeneration::value),
        // What the replacement numbers its receipts above, or `null` when no floor went
        // out (ADR-0149).
        "receipt_floor": receipt_floor,
    })
}
