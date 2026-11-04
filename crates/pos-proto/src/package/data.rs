// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A package's data part: the custom fields, object types and lists HQ declares
//! ([ADR-0194](../../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)
//! decisions 1, 2 and 5).
//!
//! These are declarations only. A value entered against one travels in an event, or, where its field
//! is personal, in the subject store; neither is declared here. Every field states whether it is
//! personal data, and nothing defaults it: [`CustomField::personal`] is absent until the author says,
//! and a field without it is refused at publish, as is free text that is not personal.
//!
//! The ceilings are ADR-0194's. `pos-core` checks them, here only named.

use serde::{Deserialize, Serialize};

use crate::ids::{CustomFieldId, CustomListId, ListEntryId, ObjectTypeId};
use crate::text::TranslationKey;
use crate::wire_enum;
use crate::wire_enum::{Open, is_absent};

/// The most custom fields on one existing object, and the most fields of one object type.
pub const FIELDS_PER_OBJECT_MAX: usize = 64;

/// The most object types a package declares.
pub const OBJECT_TYPES_MAX: usize = 32;

/// The most entries one list holds.
pub const LIST_ENTRIES_MAX: usize = 500;

/// The longest value a `text` field takes, in bytes of UTF-8.
pub const TEXT_BYTES_MAX: usize = 1024;

wire_enum! {
    /// The kind of value a custom field holds (ADR-0194 decision 2). There is no money type: an
    /// amount that matters is a core concept with its own ADR.
    CustomFieldType, prefix = "CUSTOM_FIELD_TYPE";
    /// A whole number.
    Int = "INT",
    /// Yes or no.
    Bool = "BOOL",
    /// One entry of the list the field names.
    Choice = "CHOICE",
    /// A calendar date.
    Date = "DATE",
    /// A count in thousandths of the unit the field names.
    Quantity = "QUANTITY",
    /// Free text, at most [`TEXT_BYTES_MAX`] bytes. Always personal: nobody can promise what a person
    /// will type into it.
    Text = "TEXT",
}

wire_enum! {
    /// An existing object a custom field may be added to (ADR-0194 decision 1).
    DataObject, prefix = "DATA_OBJECT";
    /// An order.
    Order = "ORDER",
    /// A line of an order.
    Line = "LINE",
    /// A bill.
    Bill = "BILL",
    /// A table.
    Table = "TABLE",
    /// A drawer's shift.
    Shift = "SHIFT",
}

/// A package's data part. Empty, and left off the wire, in a version that declares no data.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct PackageData {
    /// Custom fields on existing objects, each naming its [`CustomField::object`]: at most
    /// [`FIELDS_PER_OBJECT_MAX`] per object.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom_fields: Vec<CustomField>,
    /// New object types, at most [`OBJECT_TYPES_MAX`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub object_types: Vec<ObjectType>,
    /// Managed option lists a `choice` field draws from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lists: Vec<CustomList>,
}

impl PackageData {
    /// Whether the part declares nothing.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.custom_fields.is_empty() && self.object_types.is_empty() && self.lists.is_empty()
    }
}

/// One custom field, on an existing object or in an object type.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CustomField {
    /// The field's id, stable for good: a field is never renamed or retyped (ADR-0194 decision 6).
    pub custom_field_id: CustomFieldId,
    /// The existing object the field is on. Stated for a field of [`PackageData::custom_fields`],
    /// absent for a field of an object type.
    #[serde(default, skip_serializing_if = "is_absent")]
    pub object: Open<DataObject>,
    /// The translation key of the field's label.
    pub label_key: TranslationKey,
    /// The kind of value it holds.
    pub field_type: Open<CustomFieldType>,
    /// Whether an entry must give it a value.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub required: bool,
    /// Whether the value is personal data. Required, with no default: absent, the field is refused
    /// at publish.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub personal: Option<bool>,
    /// The list a `choice` field draws from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub custom_list_id: Option<CustomListId>,
    /// The translation key of a `quantity` field's unit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub unit_key: Option<TranslationKey>,
    /// How many days a personal value is kept. Required for a personal field, with no default
    /// (ADR-0194 decision 5); absent for any other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub retention_days: Option<u16>,
    /// The translation key of the purpose a person consents to before a personal value is saved.
    /// Required for a personal field; absent for any other.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    pub consent_purpose_key: Option<TranslationKey>,
    /// Hidden from new entries and kept for reading (ADR-0194 decision 6).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
}

/// A new kind of object, such as a reservation or a locker.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ObjectType {
    /// The type's id, stable for good.
    pub object_type_id: ObjectTypeId,
    /// The translation key of the type's name.
    pub label_key: TranslationKey,
    /// Its fields, at most [`FIELDS_PER_OBJECT_MAX`], none naming an [`CustomField::object`].
    pub fields: Vec<CustomField>,
    /// Hidden from new entries and kept for reading.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
}

/// A managed option list, like the reason codes (ADR-0115).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct CustomList {
    /// The list's id.
    pub custom_list_id: CustomListId,
    /// The translation key of the list's name in the console.
    pub label_key: TranslationKey,
    /// Its entries, at most [`LIST_ENTRIES_MAX`], in the order a till offers them.
    pub entries: Vec<ListEntry>,
}

/// One option of a list.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub struct ListEntry {
    /// The entry's id, which a `choice` value carries.
    pub list_entry_id: ListEntryId,
    /// The translation key of the entry's label.
    pub label_key: TranslationKey,
    /// No longer offered, and kept for reading.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
}

#[cfg(test)]
mod tests;
