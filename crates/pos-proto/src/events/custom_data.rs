// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The part the two custom-data events nest in their payloads: one typed value per custom field
//! ([ADR-0194](../../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)
//! decision 3).
//!
//! There is no text here, and that is the point. A `text` field is always personal, so its value
//! goes to the subject store and the event carries only the record's `subject_id` (decision 4). Every
//! value this part can hold is a number, a yes or no, a list entry's id, a date or a quantity, each
//! already proven free of personal data, so `NoPii` holds by construction.

use serde::{Deserialize, Serialize};

use crate::ids::{CustomFieldId, ListEntryId};
use crate::quantity::Quantity;
use crate::time::CalendarDate;

/// One custom field's value, as an event records it. At most one of the value fields is present,
/// the one the field's type declares; none present means the value was cleared.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
#[serde(rename_all = "snake_case")]
pub struct CustomFieldValue {
    /// The field, as the package version declares it.
    pub custom_field_id: CustomFieldId,
    /// An `int` field's value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub integer: Option<i64>,
    /// A `bool` field's value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub boolean: Option<bool>,
    /// A `choice` field's value: an entry of the field's list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub list_entry_id: Option<ListEntryId>,
    /// A `date` field's value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub calendar_date: Option<CalendarDate>,
    /// A `quantity` field's value, in thousandths of the field's unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub quantity: Option<Quantity>,
}

// The personal-data barrier for a part of a payload, field by field, and its fields in the snapshot.
crate::envelope::payload_part!(CustomFieldValue {
    custom_field_id,
    integer,
    boolean,
    list_entry_id,
    calendar_date,
    quantity,
});

#[cfg(test)]
mod tests;
