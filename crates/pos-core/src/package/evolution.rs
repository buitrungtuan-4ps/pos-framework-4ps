// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Whether a version may follow an earlier version of its package
//! ([ADR-0194](../../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)
//! decision 6): a field, object type, list or list entry is never removed, never moves and never
//! changes type, and a personal field never stops being personal. A change is deprecation, or a new
//! field.
//!
//! The cloud runs it at publish against every earlier version of the package, so no version can
//! break one that came before it.

use pos_proto::ids::{CustomFieldId, CustomListId, ObjectTypeId};
use pos_proto::package::{CustomField, PackageData};

/// Why a version cannot follow an earlier version of its package (ADR-0194 decision 6). Each names
/// what the earlier version declared, by its id's text, which is not personal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvolutionRefusal {
    /// A field was removed, moved to another object or type, retyped, given another list, or made
    /// not personal.
    Field(CustomFieldId),
    /// An object type was removed.
    ObjectType(ObjectTypeId),
    /// A list, or an entry of one, was removed.
    List(CustomListId),
}

impl EvolutionRefusal {
    /// The request field at fault, for the console's field error.
    #[must_use]
    pub const fn field(&self) -> &'static str {
        match self {
            Self::Field(_) => "data.custom_fields",
            Self::ObjectType(_) => "data.object_types",
            Self::List(_) => "data.lists",
        }
    }
}

impl core::fmt::Display for EvolutionRefusal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Field(id) => write!(
                f,
                "field {id}: an earlier version declares it, so it stays, on the same object, \
                 with the same type and list, and personal if it was"
            ),
            Self::ObjectType(id) => write!(
                f,
                "object type {id}: an earlier version declares it, so it stays; deprecate it instead"
            ),
            Self::List(id) => write!(
                f,
                "list {id}: an earlier version declares it and its entries, so they stay; \
                 deprecate an entry instead"
            ),
        }
    }
}

impl core::error::Error for EvolutionRefusal {}

/// Checks that `next` only adds to `earlier`: everything `earlier` declares is still declared, in
/// the same place and with the same type, and a personal field stays personal.
///
/// # Errors
///
/// The first [`EvolutionRefusal`] found: lists, then object types, then fields.
pub fn check_evolution(earlier: &PackageData, next: &PackageData) -> Result<(), EvolutionRefusal> {
    for list in &earlier.lists {
        let kept = next
            .lists
            .iter()
            .find(|l| l.custom_list_id == list.custom_list_id)
            .is_some_and(|kept| {
                list.entries.iter().all(|entry| {
                    kept.entries
                        .iter()
                        .any(|e| e.list_entry_id == entry.list_entry_id)
                })
            });
        if !kept {
            return Err(EvolutionRefusal::List(list.custom_list_id));
        }
    }
    for kind in &earlier.object_types {
        if !next
            .object_types
            .iter()
            .any(|t| t.object_type_id == kind.object_type_id)
        {
            return Err(EvolutionRefusal::ObjectType(kind.object_type_id));
        }
    }
    let next_fields = placed(next);
    for (home, field) in placed(earlier) {
        let kept = next_fields.iter().any(|(next_home, next_field)| {
            next_field.custom_field_id == field.custom_field_id
                && *next_home == home
                && next_field.object == field.object
                && next_field.field_type == field.field_type
                && next_field.custom_list_id == field.custom_list_id
                && (field.personal != Some(true) || next_field.personal == Some(true))
        });
        if !kept {
            return Err(EvolutionRefusal::Field(field.custom_field_id));
        }
    }
    Ok(())
}

/// Every field of `data` with the object type it is in, or `None` for a field on an existing object.
fn placed(data: &PackageData) -> Vec<(Option<ObjectTypeId>, &CustomField)> {
    data.custom_fields
        .iter()
        .map(|f| (None, f))
        .chain(
            data.object_types
                .iter()
                .flat_map(|t| t.fields.iter().map(move |f| (Some(t.object_type_id), f))),
        )
        .collect()
}

#[cfg(test)]
mod tests;
