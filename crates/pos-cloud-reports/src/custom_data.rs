// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's projection of custom data
//! ([ADR-0194](../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)
//! decision 3): one row per object and field, the value it holds now.
//!
//! [`fold_custom_data`] turns one `data.custom_field.set` or `data.custom_record.changed` event into
//! the rows it sets and clears, and [`project_custom_values`] brings one store's rows up to date
//! behind a cursor, as the rollups are ([ADR-0036](../../../docs/adr/0036-materialised-rollups.md)).
//! A row holds only what the event held: typed, non-personal values, and at most the `subject_id` of
//! the object's personal record. No personal value reaches the cloud this way (ADR-0206 decision 3).

use core::future::Future;
use core::num::NonZeroU32;

use pos_ports::PortError;
use pos_ports::event_store::{EventQuery, EventStore};
use pos_proto::envelope::{EventEnvelope, RawPayload};
use pos_proto::events::{CustomFieldValue, DataCustomFieldSet, DataCustomRecordChanged, EventType};
use pos_proto::ids::{
    CustomFieldId, EventId, ObjectTypeId, PackageVersionId, StoreId, SubjectId, TenantId,
};
use pos_proto::package::DataObject;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::wire_enum::Open;

/// How many events one projection page reads.
const PROJECT_PAGE: u32 = 512;

/// What a custom value is on: an existing object, or a record of an object type.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CustomObject {
    /// An order, line, bill, table or shift.
    Existing(Open<DataObject>),
    /// A record of a package's object type.
    Record(ObjectTypeId),
}

/// One field's current value on one object or record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomValueRow {
    /// What the value is on.
    pub object: CustomObject,
    /// The object's or record's id.
    pub object_id: Ulid,
    /// The value, with its field's id.
    pub value: CustomFieldValue,
    /// The subject record holding the object's personal values, if it has one.
    pub subject_id: Option<SubjectId>,
    /// The package version whose declarations the value follows.
    pub package_version_id: PackageVersionId,
    /// When the value was set.
    pub event_time: Timestamp,
}

/// What one event does to a store's rows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CustomValueChange {
    /// The row for this object and field now holds this value.
    Set(CustomValueRow),
    /// The value of this field on this object was cleared: its row goes.
    Cleared {
        /// The object's or record's id.
        object_id: Ulid,
        /// The field.
        custom_field_id: CustomFieldId,
    },
}

/// What an event does to the rows: nothing for any event but the two custom-data events, and
/// nothing for a payload that does not decode, as every fold skips one.
#[must_use]
pub fn fold_custom_data(event: &EventEnvelope<RawPayload>) -> Vec<CustomValueChange> {
    let (object, object_id, values, subject_id, package_version_id) = match event.event_type.known()
    {
        Some(EventType::DataCustomFieldSet) => {
            let Ok(set) = event.data.decode::<DataCustomFieldSet>() else {
                return Vec::new();
            };
            (
                CustomObject::Existing(set.object),
                set.object_id,
                set.values,
                set.subject_id,
                set.package_version_id,
            )
        }
        Some(EventType::DataCustomRecordChanged) => {
            let Ok(changed) = event.data.decode::<DataCustomRecordChanged>() else {
                return Vec::new();
            };
            (
                CustomObject::Record(changed.object_type_id),
                changed.custom_record_id.as_ulid(),
                changed.values,
                changed.subject_id,
                changed.package_version_id,
            )
        }
        _ => return Vec::new(),
    };
    values
        .into_iter()
        .map(|value| {
            if holds_nothing(&value) {
                CustomValueChange::Cleared {
                    object_id,
                    custom_field_id: value.custom_field_id,
                }
            } else {
                CustomValueChange::Set(CustomValueRow {
                    object: object.clone(),
                    object_id,
                    value,
                    subject_id,
                    package_version_id,
                    event_time: event.event_time,
                })
            }
        })
        .collect()
}

/// Whether a value names no value at all: a cleared one.
const fn holds_nothing(value: &CustomFieldValue) -> bool {
    value.integer.is_none()
        && value.boolean.is_none()
        && value.list_entry_id.is_none()
        && value.calendar_date.is_none()
        && value.quantity.is_none()
}

/// Where a store's custom values are kept, with the cursor of the last event folded into them.
///
/// The seam between the projection and the database, so the fold is tested without one.
pub trait CustomValueStore {
    /// The last event folded for the store, or `None` before the first.
    fn cursor(
        &self,
        tenant: TenantId,
        store_id: StoreId,
    ) -> impl Future<Output = Result<Option<EventId>, PortError>> + Send;

    /// Applies `changes` in order and advances the store's cursor to `cursor`, together: either
    /// both land or neither, so an event is folded exactly once.
    fn apply(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        changes: &[CustomValueChange],
        cursor: EventId,
    ) -> impl Future<Output = Result<(), PortError>> + Send;
}

/// Brings one store's custom values up to date with its log, a page at a time. Returns how many
/// events it read.
///
/// # Errors
///
/// [`PortError`] if the log or the value store cannot be read or written. What was applied before
/// the failure stays applied, with its cursor, so the next pass resumes from there.
pub async fn project_custom_values<E, V>(
    events: &E,
    values: &V,
    tenant: TenantId,
    store_id: StoreId,
) -> Result<u64, PortError>
where
    E: EventStore,
    V: CustomValueStore,
{
    let page = NonZeroU32::new(PROJECT_PAGE).unwrap_or(NonZeroU32::MIN);
    let page_len = usize::try_from(PROJECT_PAGE).unwrap_or(usize::MAX);
    let mut cursor = values.cursor(tenant, store_id).await?;
    let mut read: u64 = 0;
    loop {
        let mut query = EventQuery::first(store_id, page);
        if let Some(after) = cursor {
            query = query.after(after);
        }
        let batch = events.read(&query).await?;
        let Some(last) = batch.last().map(|event| event.event_id) else {
            break;
        };
        let changes: Vec<CustomValueChange> = batch.iter().flat_map(fold_custom_data).collect();
        values.apply(tenant, store_id, &changes, last).await?;
        read = read.saturating_add(u64::try_from(batch.len()).unwrap_or(u64::MAX));
        cursor = Some(last);
        if batch.len() < page_len {
            break;
        }
    }
    Ok(read)
}

#[cfg(test)]
mod tests;
