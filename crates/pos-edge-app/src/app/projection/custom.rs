// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! What the projection records of custom data
//! ([ADR-0194](../../../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)
//! decision 3): each object's and each record's current non-personal values, and the subject that
//! holds its personal ones. No personal value is here: the events carry none.

use std::collections::BTreeMap;

use pos_proto::events::{CustomFieldValue, DataCustomFieldSet, DataCustomRecordChanged};
use pos_proto::ids::{CustomFieldId, CustomRecordId, ObjectTypeId, PackageVersionId, SubjectId};
use pos_proto::package::DataObject;
use pos_proto::ulid::Ulid;

use crate::app::Projection;

/// The most existing objects whose custom values the projection holds. Past it the object with the
/// oldest id is dropped: an order from months ago is not edited at a till. Its values stay in the
/// log and in the cloud.
pub(in crate::app) const MAX_CUSTOM_OBJECTS: usize = 16_384;

/// The most custom records the projection holds, dropping the oldest by id past it.
pub(in crate::app) const MAX_CUSTOM_RECORDS: usize = 4_096;

/// One object's or record's custom data, as its events left it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::app) struct HeldValues {
    /// The current non-personal values, by field. A cleared value is not here.
    pub(in crate::app) values: BTreeMap<CustomFieldId, CustomFieldValue>,
    /// The subject holding the personal values, once there are any.
    pub(in crate::app) subject_id: Option<SubjectId>,
    /// The package version the last change followed.
    pub(in crate::app) package_version_id: PackageVersionId,
}

impl HeldValues {
    fn new(package_version_id: PackageVersionId) -> Self {
        Self {
            values: BTreeMap::new(),
            subject_id: None,
            package_version_id,
        }
    }

    fn apply(
        &mut self,
        values: &[CustomFieldValue],
        subject_id: Option<SubjectId>,
        package_version_id: PackageVersionId,
    ) {
        for value in values {
            let cleared = value.integer.is_none()
                && value.boolean.is_none()
                && value.list_entry_id.is_none()
                && value.calendar_date.is_none()
                && value.quantity.is_none();
            if cleared {
                self.values.remove(&value.custom_field_id);
            } else {
                self.values.insert(value.custom_field_id, value.clone());
            }
        }
        if subject_id.is_some() {
            self.subject_id = subject_id;
        }
        self.package_version_id = package_version_id;
    }
}

/// The custom data the projection holds, bounded by [`MAX_CUSTOM_OBJECTS`] and
/// [`MAX_CUSTOM_RECORDS`].
#[derive(Debug, Default)]
pub(in crate::app) struct CustomData {
    /// By object id first, so the oldest object is the first entry.
    objects: BTreeMap<(Ulid, DataObject), HeldValues>,
    records: BTreeMap<CustomRecordId, (ObjectTypeId, HeldValues)>,
}

impl Projection {
    /// Folds a `data.custom_field.set`.
    pub(in crate::app) fn fold_custom_field_set(&mut self, event: &DataCustomFieldSet) {
        let Ok(object) = event.object.require() else {
            return;
        };
        let custom = &mut self.custom;
        custom
            .objects
            .entry((event.object_id, object))
            .or_insert_with(|| HeldValues::new(event.package_version_id))
            .apply(&event.values, event.subject_id, event.package_version_id);
        while custom.objects.len() > MAX_CUSTOM_OBJECTS {
            custom.objects.pop_first();
        }
    }

    /// Folds a `data.custom_record.changed`.
    pub(in crate::app) fn fold_custom_record(&mut self, event: &DataCustomRecordChanged) {
        let custom = &mut self.custom;
        let (_, held) = custom
            .records
            .entry(event.custom_record_id)
            .or_insert_with(|| {
                (
                    event.object_type_id,
                    HeldValues::new(event.package_version_id),
                )
            });
        held.apply(&event.values, event.subject_id, event.package_version_id);
        while custom.records.len() > MAX_CUSTOM_RECORDS {
            custom.records.pop_first();
        }
    }

    /// One object's custom data, if it has any.
    pub(in crate::app) fn custom_object(
        &self,
        object: DataObject,
        object_id: Ulid,
    ) -> Option<&HeldValues> {
        self.custom.objects.get(&(object_id, object))
    }

    /// One record's object type and custom data.
    pub(in crate::app) fn custom_record(
        &self,
        custom_record_id: CustomRecordId,
    ) -> Option<&(ObjectTypeId, HeldValues)> {
        self.custom.records.get(&custom_record_id)
    }

    /// The records of one object type the projection holds, newest first, at most `limit`.
    pub(in crate::app) fn custom_records_of(
        &self,
        object_type_id: ObjectTypeId,
        limit: usize,
    ) -> Vec<(CustomRecordId, HeldValues)> {
        self.custom
            .records
            .iter()
            .rev()
            .filter(|(_, (kind, _))| *kind == object_type_id)
            .take(limit)
            .map(|(id, (_, held))| (*id, held.clone()))
            .collect()
    }
}
