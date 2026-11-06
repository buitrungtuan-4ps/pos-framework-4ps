// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Entering custom data at a till
//! ([ADR-0194](../../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md),
//! [ADR-0206](../../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)).
//!
//! A write checks its values against the package the store runs (`pos_core::package::check_entry`),
//! then commits, in one transaction, the event with the non-personal values and, when there are
//! personal ones, the subject record that holds them. The event carries only the record's
//! `subject_id`. The record expires `retention_days` after it was first collected, the days its
//! fields declare (ADR-0206 decision 1), and it holds, beside the values, when each consent was
//! given, as `consent.<purpose label key>` (decision 6).
//!
//! An edit keeps the record's clock: a value changed later expires with the rest of the record, so
//! nothing is held longer than its period from when the record began. A record already masked is
//! never written again; new personal values start a new subject.

use std::collections::BTreeMap;

use pos_core::decision::{Actor, DecisionCtx};
use pos_core::package::{CheckedEntry, Entry, EntryFault, EntryRefusal, EntryTarget, check_entry};
use pos_core::permission::Permission;
use pos_ports::TxContext;
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::{SubjectRecord, SubjectStore};
use pos_proto::events::{CustomFieldValue, DataCustomFieldSet, DataCustomRecordChanged};
use pos_proto::ids::{
    BillId, CustomRecordId, ObjectTypeId, OrderId, OrderLineId, PackageVersionId, ShiftId,
    SubjectId, TableId,
};
use pos_proto::package::{DataObject, PackageContent};
use pos_proto::text::TranslationKey;
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;
use pos_proto::{Open, envelope::EventPayload};

use crate::app::projection::HeldValues;
use crate::app::{AppError, Edge, MILLISECONDS_PER_DAY};

/// The prefix of a subject field that records when a consent was given (ADR-0206 decision 6).
pub const CONSENT_FIELD_PREFIX: &str = "consent.";

/// One object's or record's custom data as the edge holds it: its non-personal values and the
/// subject holding its personal ones. Never a personal value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CustomDataView {
    /// The record, for a custom record; `None` for fields on an existing object.
    pub custom_record_id: Option<CustomRecordId>,
    /// The record's object type, for a custom record.
    pub object_type_id: Option<ObjectTypeId>,
    /// The current non-personal values, by field.
    pub values: Vec<CustomFieldValue>,
    /// The subject holding the personal values, once there are any.
    pub subject_id: Option<SubjectId>,
    /// The package version the last change followed.
    pub package_version_id: PackageVersionId,
}

impl CustomDataView {
    fn of(record: Option<(CustomRecordId, ObjectTypeId)>, held: &HeldValues) -> Self {
        Self {
            custom_record_id: record.map(|(id, _)| id),
            object_type_id: record.map(|(_, kind)| kind),
            values: held.values.values().cloned().collect(),
            subject_id: held.subject_id,
            package_version_id: held.package_version_id,
        }
    }
}

/// What a personal write commits beside its event: the subject and its record.
type SubjectWrite = Option<(SubjectId, SubjectRecord)>;

impl<S: EventStore + SubjectStore> Edge<S> {
    /// Sets custom fields on an existing object: an order, a line, a bill, a table or a shift.
    ///
    /// Needs `data.record.write` and the object's own permission (ADR-0206 decision 7): an order's
    /// or a line's `sales.line.add`, a bill's `billing.bill.open`, a table's `sales.table.manage`,
    /// a shift's `cash.shift.open`.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] for a missing permission, [`AppError::UnknownObject`] for an object
    /// this edge does not know, [`AppError::CustomEntryRefused`] for values that do not fit the
    /// package, or [`AppError`] if the store cannot be read or written.
    pub async fn set_custom_fields(
        &self,
        actor: Actor,
        object: DataObject,
        object_id: Ulid,
        entries: &[Entry],
        consented: &[TranslationKey],
    ) -> Result<CustomDataView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        ctx.require(Permission::WriteDataRecord)?;
        ctx.require(object_permission(object))?;
        if !self.knows_object(object, object_id) {
            return Err(AppError::UnknownObject);
        }
        let package = self.running_package()?;
        let checked = check_entry(
            &package.data,
            EntryTarget::Object(object),
            entries,
            consented,
        )
        .map_err(AppError::CustomEntryRefused)?;
        let held = self
            .lock_projection()
            .custom_object(object, object_id)
            .and_then(|held| held.subject_id);
        let subject = self.personal_write(&ctx, held, &checked).await?;
        let event = DataCustomFieldSet {
            object: Open::from_known(object),
            object_id,
            values: checked.values,
            subject_id: subject.as_ref().map(|(id, _)| *id).or(held),
            package_version_id: package.package_version_id,
        };
        self.commit_custom(&ctx, &event, subject).await?;
        let mut projection = self.lock_projection();
        projection.fold_custom_field_set(&event);
        let held = projection
            .custom_object(object, object_id)
            .ok_or(AppError::UnknownObject)?;
        Ok(CustomDataView::of(None, held))
    }

    /// Creates a record of a custom object type, when `custom_record_id` is `None`, or changes one.
    /// Needs `data.record.write`.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] for a missing permission, [`AppError::UnknownCustomRecord`] for a
    /// record this edge does not hold or one of another type, [`AppError::CustomEntryRefused`] for
    /// values that do not fit the package, or [`AppError`] if the store cannot be read or written.
    pub async fn save_custom_record(
        &self,
        actor: Actor,
        object_type_id: ObjectTypeId,
        custom_record_id: Option<CustomRecordId>,
        entries: &[Entry],
        consented: &[TranslationKey],
    ) -> Result<CustomDataView, AppError> {
        let ctx = self.decision_ctx(actor)?;
        ctx.require(Permission::WriteDataRecord)?;
        let held = match custom_record_id {
            Some(id) => {
                let projection = self.lock_projection();
                let (kind, held) = projection
                    .custom_record(id)
                    .ok_or(AppError::UnknownCustomRecord)?;
                if *kind != object_type_id {
                    return Err(AppError::UnknownCustomRecord);
                }
                held.subject_id
            }
            None => None,
        };
        let package = self.running_package()?;
        let target = EntryTarget::Record {
            object_type_id: &object_type_id,
            creating: custom_record_id.is_none(),
        };
        let checked = check_entry(&package.data, target, entries, consented)
            .map_err(AppError::CustomEntryRefused)?;
        let subject = self.personal_write(&ctx, held, &checked).await?;
        let custom_record_id =
            custom_record_id.unwrap_or_else(|| CustomRecordId::new(self.next_ulid()));
        let event = DataCustomRecordChanged {
            custom_record_id,
            object_type_id,
            values: checked.values,
            subject_id: subject.as_ref().map(|(id, _)| *id).or(held),
            package_version_id: package.package_version_id,
        };
        self.commit_custom(&ctx, &event, subject).await?;
        let mut projection = self.lock_projection();
        projection.fold_custom_record(&event);
        let (_, held) = projection
            .custom_record(custom_record_id)
            .ok_or(AppError::UnknownCustomRecord)?;
        Ok(CustomDataView::of(
            Some((custom_record_id, object_type_id)),
            held,
        ))
    }

    /// One object's custom data, if it has any.
    #[must_use]
    pub fn custom_object(&self, object: DataObject, object_id: Ulid) -> Option<CustomDataView> {
        self.lock_projection()
            .custom_object(object, object_id)
            .map(|held| CustomDataView::of(None, held))
    }

    /// One custom record, if this edge holds it.
    #[must_use]
    pub fn custom_record(&self, custom_record_id: CustomRecordId) -> Option<CustomDataView> {
        self.lock_projection()
            .custom_record(custom_record_id)
            .map(|(kind, held)| CustomDataView::of(Some((custom_record_id, *kind)), held))
    }

    /// The records of one object type this edge holds, newest first, at most `limit`.
    #[must_use]
    pub fn custom_records(
        &self,
        object_type_id: ObjectTypeId,
        limit: usize,
    ) -> Vec<CustomDataView> {
        self.lock_projection()
            .custom_records_of(object_type_id, limit)
            .iter()
            .map(|(id, held)| CustomDataView::of(Some((*id, object_type_id)), held))
            .collect()
    }

    /// The package the store runs, whose declarations an entry follows.
    fn running_package(&self) -> Result<std::sync::Arc<PackageContent>, AppError> {
        self.session()
            .package
            .clone()
            .ok_or(AppError::CustomEntryRefused(EntryRefusal {
                custom_field_id: None,
                fault: EntryFault::UnknownObjectType,
            }))
    }

    /// Whether this edge knows the object a custom field is set on.
    fn knows_object(&self, object: DataObject, object_id: Ulid) -> bool {
        let projection = self.lock_projection();
        match object {
            DataObject::Order => projection.order_origin(OrderId::new(object_id)).is_some(),
            DataObject::Line => projection.line(OrderLineId::new(object_id)).is_some(),
            DataObject::Bill => projection.bill(BillId::new(object_id)).is_some(),
            DataObject::Shift => projection.shift(ShiftId::new(object_id)).is_some(),
            DataObject::Table => {
                let table_id = TableId::new(object_id);
                drop(projection);
                self.session().floor.table(table_id).is_some()
            }
            DataObject::Unspecified => false,
        }
    }

    /// The subject record an entry's personal values go to, or `None` when it sets none and
    /// changes no record.
    ///
    /// The object's own subject while it holds unmasked data, keeping its clock; otherwise a new
    /// subject collected now, kept for the declared days.
    async fn personal_write(
        &self,
        ctx: &DecisionCtx,
        held: Option<SubjectId>,
        checked: &CheckedEntry,
    ) -> Result<SubjectWrite, AppError> {
        if checked.personal.is_empty() {
            return Ok(None);
        }
        let store_id = self.identity.store_id;
        let existing = match held {
            Some(subject_id) => self
                .store
                .fetch(store_id, subject_id)
                .await?
                .filter(|record| !record.is_masked())
                .map(|record| (subject_id, record)),
            None => None,
        };
        let (subject_id, mut record) = if let Some(found) = existing {
            found
        } else {
            if checked.personal.values().all(Option::is_none) {
                return Ok(None);
            }
            let days = i64::from(checked.retention_days.unwrap_or(0));
            let until = ctx
                .now
                .as_milliseconds_since_epoch()
                .saturating_add(days.saturating_mul(MILLISECONDS_PER_DAY));
            let until = Timestamp::from_milliseconds_since_epoch(until).unwrap_or(ctx.now);
            let fresh = SubjectRecord::new(ctx.now, BTreeMap::new()).retained_until(until);
            (SubjectId::new(self.next_ulid()), fresh)
        };
        for (field, value) in &checked.personal {
            match value {
                Some(text) => {
                    record
                        .fields
                        .insert(field.to_string(), text.as_str().to_owned());
                }
                None => {
                    record.fields.remove(&field.to_string());
                }
            }
        }
        for purpose in &checked.consent_purpose_keys {
            record.fields.insert(
                format!("{CONSENT_FIELD_PREFIX}{purpose}"),
                ctx.now.to_string(),
            );
        }
        Ok(Some((subject_id, record)))
    }

    /// Commits a custom-data event and, where there is one, its subject record in one transaction
    /// (ADR-0107's rule: the record and the event that names it land together or not at all), then
    /// tells every device.
    async fn commit_custom<P: EventPayload + serde::Serialize>(
        &self,
        ctx: &DecisionCtx,
        event: &P,
        subject: SubjectWrite,
    ) -> Result<(), AppError> {
        let (envelope, message) = self.prepare(ctx, event)?;
        let mut tx = self.store.begin().await?;
        self.store.append(&mut tx, &[envelope]).await?;
        if let Some((subject_id, record)) = subject {
            self.store
                .record(&mut tx, self.identity.store_id, subject_id, &record)
                .await?;
        }
        tx.commit().await?;
        self.fanout.publish(&message);
        Ok(())
    }
}

/// The permission an existing object is edited under (ADR-0206 decision 7).
const fn object_permission(object: DataObject) -> Permission {
    match object {
        DataObject::Order | DataObject::Line | DataObject::Unspecified => Permission::AddLine,
        DataObject::Bill => Permission::OpenBill,
        DataObject::Table => Permission::ManageTables,
        DataObject::Shift => Permission::OpenShift,
    }
}

#[cfg(test)]
mod tests;
