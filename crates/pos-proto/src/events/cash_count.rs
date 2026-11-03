// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The part a drawer count nests in its payload: one row per note or coin counted
//! ([ADR-0186](../../../../docs/adr/0186-a-drawer-is-counted-by-denomination.md)).

use serde::{Deserialize, Serialize};

use crate::money::Money;

/// How many of one note or coin were counted in a drawer: money and a quantity, and nothing about
/// who counted it.
///
/// `cash.shift.opened` and `cash.shift.counted` carry a list of these beside an `uncounted_amount`
/// for anything the list does not name; together they add up to the amount the event records.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct DenominationCount {
    /// The note or coin, in the drawer's currency.
    pub denomination: Money,
    /// How many of it were counted.
    pub quantity: u32,
}

// The personal-data barrier for a part of a payload, field by field, and its fields in the snapshot.
crate::envelope::payload_part!(DenominationCount {
    denomination,
    quantity,
});
