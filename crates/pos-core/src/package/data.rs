// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Whether a package's data part may be published and applied
//! ([ADR-0194](../../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md),
//! [ADR-0206](../../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)).
//!
//! [`check_data`] checks one version on its own.
//!
//! A refusal names where the fault is by position, never by value, so it can be shown and logged.

use pos_proto::ids::CustomFieldId;
use pos_proto::package::{
    CustomField, CustomFieldType, DataObject, FIELDS_PER_OBJECT_MAX, LIST_ENTRIES_MAX,
    OBJECT_TYPES_MAX, PackageData,
};
use pos_proto::text::TranslationKey;
use pos_proto::wire_enum::{Open, WireEnum};

/// Whether a field may be declared personal yet. Off until the store keeps each personal record
/// for its own period and an erasure reaches it (ADR-0206 decision 11).
pub const PERSONAL_FIELDS_AVAILABLE: bool = false;

/// Where a refused field is: the `n`th of the data part's custom fields, or the `field`th field of
/// the `object_type`th object type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldAt {
    /// A field on an existing object, by its position in `custom_fields`.
    Object(usize),
    /// A field of an object type, by the type's position and the field's.
    Type {
        /// The object type's position in `object_types`.
        object_type: usize,
        /// The field's position in the type's `fields`.
        field: usize,
    },
}

/// What is wrong with a field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldFault {
    /// Another field of the package has the same id.
    DuplicateId,
    /// A field on an existing object names none, or one this release does not know; or a field of
    /// an object type names one.
    Object,
    /// The label key is not 1 to 128 characters of `a`–`z`, `0`–`9`, `_` and `.`.
    LabelKey,
    /// The type is absent or unknown to this release.
    FieldType,
    /// A `choice` field names no declared list, or another type names one.
    List,
    /// A `quantity` field names no unit key, or another type names one.
    Unit,
    /// `personal` is absent: nothing defaults it.
    PersonalUndeclared,
    /// A `text` field is declared not personal. Free text is always personal.
    TextNotPersonal,
    /// A personal field names no retention period of at least a day, or a field that is not
    /// personal names one.
    Retention,
    /// A personal field names no consent purpose, or a field that is not personal names one.
    Consent,
    /// A personal field on a shift: no record is kept about staff (ADR-0206 decision 8).
    PersonalOnStaff,
    /// A personal field whose retention period differs from another personal field of the same
    /// object or type (ADR-0206 decision 2).
    RetentionDisagrees,
    /// A personal field, before personal fields are available (ADR-0206 decision 11).
    PersonalUnavailable,
}

/// Why a data part cannot be published or applied.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataRefusal {
    /// One object has more than [`FIELDS_PER_OBJECT_MAX`] custom fields, or one object type more
    /// than that many fields.
    TooManyFields,
    /// More than [`OBJECT_TYPES_MAX`] object types.
    TooManyObjectTypes,
    /// An object type's id is taken by an earlier type, or its label key is malformed.
    ObjectType(usize),
    /// A list's id is taken by an earlier list, its label key is malformed, it has no entry or more
    /// than [`LIST_ENTRIES_MAX`], or two of its entries share an id or one has a malformed key.
    List(usize),
    /// A field, and what is wrong with it.
    Field(FieldAt, FieldFault),
}

impl DataRefusal {
    /// The request field at fault, for the console's field error.
    #[must_use]
    pub const fn field(self) -> &'static str {
        match self {
            Self::TooManyFields | Self::TooManyObjectTypes | Self::ObjectType(_) => {
                "data.object_types"
            }
            Self::List(_) => "data.lists",
            Self::Field(FieldAt::Object(_), fault) => match fault {
                FieldFault::PersonalUndeclared
                | FieldFault::TextNotPersonal
                | FieldFault::PersonalOnStaff
                | FieldFault::PersonalUnavailable => "data.custom_fields.personal",
                FieldFault::Retention | FieldFault::RetentionDisagrees => {
                    "data.custom_fields.retention_days"
                }
                FieldFault::DuplicateId
                | FieldFault::Object
                | FieldFault::LabelKey
                | FieldFault::FieldType
                | FieldFault::List
                | FieldFault::Unit
                | FieldFault::Consent => "data.custom_fields",
            },
            Self::Field(FieldAt::Type { .. }, fault) => match fault {
                FieldFault::PersonalUndeclared
                | FieldFault::TextNotPersonal
                | FieldFault::PersonalOnStaff
                | FieldFault::PersonalUnavailable => "data.object_types.fields.personal",
                FieldFault::Retention | FieldFault::RetentionDisagrees => {
                    "data.object_types.fields.retention_days"
                }
                FieldFault::DuplicateId
                | FieldFault::Object
                | FieldFault::LabelKey
                | FieldFault::FieldType
                | FieldFault::List
                | FieldFault::Unit
                | FieldFault::Consent => "data.object_types.fields",
            },
        }
    }

    /// The code the console's field error carries: `PERSONAL_FIELD_UNAVAILABLE` for a personal field
    /// refused because personal fields are not available yet, `INVALID` for every other fault.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Field(_, FieldFault::PersonalUnavailable) => "PERSONAL_FIELD_UNAVAILABLE",
            Self::TooManyFields
            | Self::TooManyObjectTypes
            | Self::ObjectType(_)
            | Self::List(_)
            | Self::Field(..) => "INVALID",
        }
    }
}

impl core::fmt::Display for DataRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::TooManyFields => write!(
                f,
                "an object or object type has at most {FIELDS_PER_OBJECT_MAX} custom fields"
            ),
            Self::TooManyObjectTypes => {
                write!(f, "a package has at most {OBJECT_TYPES_MAX} object types")
            }
            Self::ObjectType(index) => write!(
                f,
                "object type {index}: its id is unique and its label key well formed"
            ),
            Self::List(index) => write!(
                f,
                "list {index}: its id is unique, its label keys well formed, and it has 1 to \
                 {LIST_ENTRIES_MAX} entries with unique ids"
            ),
            Self::Field(at, fault) => {
                match at {
                    FieldAt::Object(index) => write!(f, "custom field {index}: ")?,
                    FieldAt::Type { object_type, field } => {
                        write!(f, "object type {object_type}, field {field}: ")?;
                    }
                }
                f.write_str(fault_text(*fault))
            }
        }
    }
}

const fn fault_text(fault: FieldFault) -> &'static str {
    match fault {
        FieldFault::DuplicateId => "another field has the same id",
        FieldFault::Object => {
            "a field on an existing object names a known object, and a field of a type names none"
        }
        FieldFault::LabelKey => "the label key is malformed",
        FieldFault::FieldType => "the type is absent or unknown",
        FieldFault::List => "a choice field names a declared list, and no other field names one",
        FieldFault::Unit => "a quantity field names a unit key, and no other field names one",
        FieldFault::PersonalUndeclared => "it must say whether it is personal",
        FieldFault::TextNotPersonal => "free text is always personal",
        FieldFault::Retention => {
            "a personal field names a retention period of at least a day, and no other field does"
        }
        FieldFault::Consent => "a personal field names a consent purpose, and no other field does",
        FieldFault::PersonalOnStaff => "a shift takes no personal field",
        FieldFault::RetentionDisagrees => {
            "the personal fields of one object or type share one retention period"
        }
        FieldFault::PersonalUnavailable => "personal fields are not available yet",
    }
}

impl core::error::Error for DataRefusal {}

/// Checks a version's data part on its own.
///
/// # Errors
///
/// The first [`DataRefusal`] found: the lists, then the fields on existing objects, then the
/// object types and their fields.
pub fn check_data(data: &PackageData) -> Result<(), DataRefusal> {
    for (index, list) in data.lists.iter().enumerate() {
        let taken = data
            .lists
            .iter()
            .take(index)
            .any(|earlier| earlier.custom_list_id == list.custom_list_id);
        let entries_fine = !list.entries.is_empty()
            && list.entries.len() <= LIST_ENTRIES_MAX
            && list.entries.iter().enumerate().all(|(at, entry)| {
                is_label_key(&entry.label_key)
                    && list
                        .entries
                        .iter()
                        .take(at)
                        .all(|earlier| earlier.list_entry_id != entry.list_entry_id)
            });
        if taken || !is_label_key(&list.label_key) || !entries_fine {
            return Err(DataRefusal::List(index));
        }
    }
    for object in DataObject::ALL
        .iter()
        .filter(|o| **o != DataObject::Unspecified)
    {
        let on_it = || {
            data.custom_fields
                .iter()
                .enumerate()
                .filter(|(_, f)| f.object.known() == *object)
        };
        if on_it().count() > FIELDS_PER_OBJECT_MAX {
            return Err(DataRefusal::TooManyFields);
        }
        if let Some(index) = disagreeing(on_it()) {
            return Err(DataRefusal::Field(
                FieldAt::Object(index),
                FieldFault::RetentionDisagrees,
            ));
        }
    }
    let mut seen: Vec<CustomFieldId> = Vec::new();
    for (index, field) in data.custom_fields.iter().enumerate() {
        check_field(data, field, true, &mut seen)
            .map_err(|fault| DataRefusal::Field(FieldAt::Object(index), fault))?;
    }
    if data.object_types.len() > OBJECT_TYPES_MAX {
        return Err(DataRefusal::TooManyObjectTypes);
    }
    for (object_type, kind) in data.object_types.iter().enumerate() {
        let taken = data
            .object_types
            .iter()
            .take(object_type)
            .any(|earlier| earlier.object_type_id == kind.object_type_id);
        if taken || !is_label_key(&kind.label_key) {
            return Err(DataRefusal::ObjectType(object_type));
        }
        if kind.fields.len() > FIELDS_PER_OBJECT_MAX {
            return Err(DataRefusal::TooManyFields);
        }
        if let Some(field) = disagreeing(kind.fields.iter().enumerate()) {
            return Err(DataRefusal::Field(
                FieldAt::Type { object_type, field },
                FieldFault::RetentionDisagrees,
            ));
        }
        for (field_at, field) in kind.fields.iter().enumerate() {
            check_field(data, field, false, &mut seen).map_err(|fault| {
                DataRefusal::Field(
                    FieldAt::Type {
                        object_type,
                        field: field_at,
                    },
                    fault,
                )
            })?;
        }
    }
    Ok(())
}

/// The position of the first personal field whose retention period differs from the first
/// personal field's, if any.
fn disagreeing<'a>(fields: impl Iterator<Item = (usize, &'a CustomField)>) -> Option<usize> {
    let mut personal = fields.filter(|(_, f)| f.personal == Some(true));
    let (_, first) = personal.next()?;
    personal
        .find(|(_, f)| f.retention_days != first.retention_days)
        .map(|(index, _)| index)
}

fn check_field(
    data: &PackageData,
    field: &CustomField,
    on_object: bool,
    seen: &mut Vec<CustomFieldId>,
) -> Result<(), FieldFault> {
    if seen.contains(&field.custom_field_id) {
        return Err(FieldFault::DuplicateId);
    }
    seen.push(field.custom_field_id);
    let object = field.object.known();
    let object_fine = if on_object {
        object != DataObject::Unspecified
    } else {
        field.object == Open::default()
    };
    if !object_fine {
        return Err(FieldFault::Object);
    }
    if !is_label_key(&field.label_key) {
        return Err(FieldFault::LabelKey);
    }
    let field_type = field.field_type.known();
    if field_type == CustomFieldType::Unspecified {
        return Err(FieldFault::FieldType);
    }
    let list_fine = match field.custom_list_id {
        Some(list) => {
            field_type == CustomFieldType::Choice
                && data.lists.iter().any(|l| l.custom_list_id == list)
        }
        None => field_type != CustomFieldType::Choice,
    };
    if !list_fine {
        return Err(FieldFault::List);
    }
    let unit_fine = match &field.unit_key {
        Some(key) => field_type == CustomFieldType::Quantity && is_label_key(key),
        None => field_type != CustomFieldType::Quantity,
    };
    if !unit_fine {
        return Err(FieldFault::Unit);
    }
    check_personal(field, field_type, object)
}

fn check_personal(
    field: &CustomField,
    field_type: CustomFieldType,
    object: DataObject,
) -> Result<(), FieldFault> {
    let Some(personal) = field.personal else {
        return Err(FieldFault::PersonalUndeclared);
    };
    if !personal {
        return if field_type == CustomFieldType::Text {
            Err(FieldFault::TextNotPersonal)
        } else if field.retention_days.is_some() {
            Err(FieldFault::Retention)
        } else if field.consent_purpose_key.is_some() {
            Err(FieldFault::Consent)
        } else {
            Ok(())
        };
    }
    if field.retention_days.is_none_or(|days| days == 0) {
        return Err(FieldFault::Retention);
    }
    if !field.consent_purpose_key.as_ref().is_some_and(is_label_key) {
        return Err(FieldFault::Consent);
    }
    if object == DataObject::Shift {
        return Err(FieldFault::PersonalOnStaff);
    }
    if PERSONAL_FIELDS_AVAILABLE {
        Ok(())
    } else {
        Err(FieldFault::PersonalUnavailable)
    }
}

fn is_label_key(key: &TranslationKey) -> bool {
    let key = key.as_str();
    !key.is_empty()
        && key.chars().count() <= super::LABEL_KEY_CHARS_MAX
        && key
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '.')
}

#[cfg(test)]
mod tests;
