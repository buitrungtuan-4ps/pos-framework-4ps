// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Whether values entered at a till fit the package's declarations, and which of them are personal
//! ([ADR-0194](../../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)
//! decisions 2 to 5,
//! [ADR-0206](../../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)
//! decisions 1, 2 and 6).
//!
//! [`check_entry`] splits an entry in two: the non-personal values, typed, for the event, and the
//! personal values, as text, for the subject store. A personal value is never in the first half, so
//! an event built from it cannot carry one.
//!
//! A refusal names the field by id and says what is wrong, never the value, so it can be shown and
//! logged.

use std::collections::BTreeMap;

use pos_proto::events::CustomFieldValue;
use pos_proto::ids::{CustomFieldId, ListEntryId, ObjectTypeId};
use pos_proto::package::{CustomField, CustomFieldType, DataObject, PackageData, TEXT_BYTES_MAX};
use pos_proto::quantity::Quantity;
use pos_proto::text::TranslationKey;
use pos_proto::time::CalendarDate;

/// Free text a person typed. Always personal, so its `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct PersonalText(String);

impl PersonalText {
    /// Wraps typed text.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        Self(text.into())
    }

    /// The text.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl core::fmt::Debug for PersonalText {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("PersonalText(..)")
    }
}

/// One value as a till entered it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnteredValue {
    /// The value is cleared.
    Cleared,
    /// An `int` field's value.
    Integer(i64),
    /// A `bool` field's value.
    Boolean(bool),
    /// A `choice` field's value.
    ListEntry(ListEntryId),
    /// A `date` field's value.
    Date(CalendarDate),
    /// A `quantity` field's value, in thousandths of its unit.
    Quantity(Quantity),
    /// A `text` field's value.
    Text(PersonalText),
}

/// One field's entered value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The field.
    pub custom_field_id: CustomFieldId,
    /// Its value.
    pub value: EnteredValue,
}

/// What the entry is for: custom fields on an existing object, or a record of an object type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryTarget<'a> {
    /// Fields on an existing object of this kind.
    Object(DataObject),
    /// A record of this object type. `creating` is true for its first entry.
    Record {
        /// The object type.
        object_type_id: &'a ObjectTypeId,
        /// Whether the record is new.
        creating: bool,
    },
}

/// An entry that fits its declarations, split by whether each value is personal.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckedEntry {
    /// The non-personal values, for the event: none of them personal.
    pub values: Vec<CustomFieldValue>,
    /// The personal values by field id, as text for the subject store; `None` clears one.
    pub personal: BTreeMap<CustomFieldId, Option<PersonalText>>,
    /// How many days the target's personal values are kept, when it declares any personal field.
    /// One period per object or type (ADR-0206 decision 2).
    pub retention_days: Option<u16>,
    /// The consent purposes the personal values set need, each once.
    pub consent_purpose_keys: Vec<TranslationKey>,
}

/// What is wrong with an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryFault {
    /// The package declares no such object type, or none for new entries.
    UnknownObjectType,
    /// The target declares no such field.
    UnknownField,
    /// The field is deprecated and takes no new value.
    Deprecated,
    /// The field is named twice.
    Repeated,
    /// The value is not of the field's type.
    WrongType,
    /// A `choice` value is not a current entry of the field's list.
    UnknownListEntry,
    /// A `text` value is longer than [`TEXT_BYTES_MAX`] bytes.
    TextTooLong,
    /// A required field has no value.
    Required,
    /// A personal value without its consent recorded (ADR-0194 decision 5).
    ConsentMissing,
}

/// Why an entry is refused: the field, where one is at fault, and what is wrong.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EntryRefusal {
    /// The field at fault, or `None` for the object type.
    pub custom_field_id: Option<CustomFieldId>,
    /// What is wrong.
    pub fault: EntryFault,
}

impl core::fmt::Display for EntryRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if let Some(field) = self.custom_field_id {
            write!(f, "custom field {field}: ")?;
        }
        f.write_str(match self.fault {
            EntryFault::UnknownObjectType => "the package declares no such object type",
            EntryFault::UnknownField => "the package declares no such field here",
            EntryFault::Deprecated => "the field is deprecated and takes no new value",
            EntryFault::Repeated => "the field is named more than once",
            EntryFault::WrongType => "the value is not of the field's type",
            EntryFault::UnknownListEntry => "the value is not an entry of the field's list",
            EntryFault::TextTooLong => "the text is too long",
            EntryFault::Required => "the field is required",
            EntryFault::ConsentMissing => "the value needs its consent recorded first",
        })
    }
}

impl core::error::Error for EntryRefusal {}

/// Checks an entry against the package's data part, and splits it into its non-personal values and
/// its personal ones.
///
/// `consented` are the consent purposes the person agreed to on this entry's screen. A personal
/// value set needs its field's purpose among them; clearing one needs none.
///
/// # Errors
///
/// The first [`EntryRefusal`] found, in the entry's order, then a required field left without a
/// value.
pub fn check_entry(
    data: &PackageData,
    target: EntryTarget<'_>,
    entries: &[Entry],
    consented: &[TranslationKey],
) -> Result<CheckedEntry, EntryRefusal> {
    let fields: Vec<&CustomField> = match target {
        EntryTarget::Object(object) => data
            .custom_fields
            .iter()
            .filter(|field| field.object.known() == object)
            .collect(),
        EntryTarget::Record {
            object_type_id,
            creating,
        } => {
            let kind = data
                .object_types
                .iter()
                .find(|kind| kind.object_type_id == *object_type_id)
                .filter(|kind| !(creating && kind.deprecated))
                .ok_or(EntryRefusal {
                    custom_field_id: None,
                    fault: EntryFault::UnknownObjectType,
                })?;
            kind.fields.iter().collect()
        }
    };
    let mut checked = CheckedEntry {
        retention_days: fields
            .iter()
            .find(|field| field.personal == Some(true))
            .and_then(|field| field.retention_days),
        ..CheckedEntry::default()
    };
    for (at, entry) in entries.iter().enumerate() {
        let refuse = |fault| EntryRefusal {
            custom_field_id: Some(entry.custom_field_id),
            fault,
        };
        let field = fields
            .iter()
            .find(|field| field.custom_field_id == entry.custom_field_id)
            .ok_or(refuse(EntryFault::UnknownField))?;
        if entries
            .iter()
            .take(at)
            .any(|earlier| earlier.custom_field_id == entry.custom_field_id)
        {
            return Err(refuse(EntryFault::Repeated));
        }
        let cleared = entry.value == EnteredValue::Cleared;
        if field.deprecated && !cleared {
            return Err(refuse(EntryFault::Deprecated));
        }
        if field.required && cleared {
            return Err(refuse(EntryFault::Required));
        }
        let typed = typed(data, field, &entry.value).map_err(refuse)?;
        if field.personal == Some(true) {
            let text = typed.map(as_text);
            if text.is_some()
                && let Some(purpose) = &field.consent_purpose_key
            {
                if !consented.contains(purpose) {
                    return Err(refuse(EntryFault::ConsentMissing));
                }
                if !checked.consent_purpose_keys.contains(purpose) {
                    checked.consent_purpose_keys.push(purpose.clone());
                }
            }
            checked.personal.insert(entry.custom_field_id, text);
        } else {
            checked
                .values
                .push(as_event_value(entry.custom_field_id, typed));
        }
    }
    if let EntryTarget::Record { creating: true, .. } = target
        && let Some(missing) = fields.iter().find(|field| {
            field.required
                && !field.deprecated
                && !entries
                    .iter()
                    .any(|entry| entry.custom_field_id == field.custom_field_id)
        })
    {
        return Err(EntryRefusal {
            custom_field_id: Some(missing.custom_field_id),
            fault: EntryFault::Required,
        });
    }
    Ok(checked)
}

/// A value checked against its field's type: `None` for a cleared one.
type Typed<'v> = Option<&'v EnteredValue>;

fn typed<'v>(
    data: &PackageData,
    field: &CustomField,
    value: &'v EnteredValue,
) -> Result<Typed<'v>, EntryFault> {
    let fits = match (field.field_type.known(), value) {
        (_, EnteredValue::Cleared) => return Ok(None),
        (CustomFieldType::Int, EnteredValue::Integer(_))
        | (CustomFieldType::Bool, EnteredValue::Boolean(_))
        | (CustomFieldType::Date, EnteredValue::Date(_))
        | (CustomFieldType::Quantity, EnteredValue::Quantity(_)) => true,
        (CustomFieldType::Choice, EnteredValue::ListEntry(entry)) => {
            let offered = data
                .lists
                .iter()
                .find(|list| Some(list.custom_list_id) == field.custom_list_id)
                .is_some_and(|list| {
                    list.entries
                        .iter()
                        .any(|e| e.list_entry_id == *entry && !e.deprecated)
                });
            if !offered {
                return Err(EntryFault::UnknownListEntry);
            }
            true
        }
        (CustomFieldType::Text, EnteredValue::Text(text)) => {
            if text.as_str().len() > TEXT_BYTES_MAX {
                return Err(EntryFault::TextTooLong);
            }
            // Empty text is no value: it clears the field rather than storing nothing.
            if text.as_str().trim().is_empty() {
                return if field.required {
                    Err(EntryFault::Required)
                } else {
                    Ok(None)
                };
            }
            true
        }
        _ => false,
    };
    if fits {
        Ok(Some(value))
    } else {
        Err(EntryFault::WrongType)
    }
}

/// A personal value as the subject store holds it: text, whatever its type.
fn as_text(value: &EnteredValue) -> PersonalText {
    PersonalText::new(match value {
        EnteredValue::Cleared => String::new(),
        EnteredValue::Integer(integer) => integer.to_string(),
        EnteredValue::Boolean(boolean) => boolean.to_string(),
        EnteredValue::ListEntry(entry) => entry.to_string(),
        EnteredValue::Date(date) => date.to_string(),
        EnteredValue::Quantity(quantity) => quantity.as_milli().to_string(),
        EnteredValue::Text(text) => text.as_str().to_owned(),
    })
}

/// A non-personal value as an event records it. A `text` value never reaches here: free text is
/// always personal, which publish enforces.
fn as_event_value(custom_field_id: CustomFieldId, typed: Typed<'_>) -> CustomFieldValue {
    let mut value = CustomFieldValue {
        custom_field_id,
        integer: None,
        boolean: None,
        list_entry_id: None,
        calendar_date: None,
        quantity: None,
    };
    match typed {
        Some(EnteredValue::Integer(integer)) => value.integer = Some(*integer),
        Some(EnteredValue::Boolean(boolean)) => value.boolean = Some(*boolean),
        Some(EnteredValue::ListEntry(entry)) => value.list_entry_id = Some(*entry),
        Some(EnteredValue::Date(date)) => value.calendar_date = Some(*date),
        Some(EnteredValue::Quantity(quantity)) => value.quantity = Some(*quantity),
        Some(EnteredValue::Cleared | EnteredValue::Text(_)) | None => {}
    }
    value
}

#[cfg(test)]
mod tests;
