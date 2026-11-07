// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The custom fields a receipt template prints, read for one document
//! ([ADR-0207](../../../../docs/adr/0207-a-receipt-is-a-structured-template-per-store-and-channel.md)
//! decisions 3 and 5, [ADR-0206](../../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)
//! decision 9).
//!
//! A field prints as its label and value, in the template's order. A field the running package does
//! not declare on one of the document's objects is skipped, and so is one with no value: the till
//! never refuses to print because of a template. A personal value is read from the subject store, on
//! this store, only for the person's own copy; on any other document its label prints with no value.

use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;
use pos_proto::SalesChannel;
use pos_proto::events::CustomFieldValue;
use pos_proto::ids::{BillId, CustomFieldId, CustomRecordId, OrderId, SubjectId};
use pos_proto::package::{CustomField, CustomFieldType, DataObject, PackageContent};
use pos_proto::quantity::Quantity;
use pos_proto::text::TranslationKey;
use pos_proto::time::CalendarDate;
use pos_proto::ulid::Ulid;

use crate::app::{CustomDataView, Edge};

/// What a document is about, whose custom fields it may print.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PrintedObjects {
    /// A bill and the order it charges: a receipt and its copy.
    Bill(BillId),
    /// An order not yet settled: a pre-bill.
    Order(OrderId),
    /// A custom record: its slip.
    Record(CustomRecordId),
}

/// Whether a document may print a personal value (ADR-0207 decision 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersonalOnPaper {
    /// Not the person's own copy: the label prints, the value does not.
    Withheld,
    /// The person's own copy, printed as the act that made it: the record's slip, or the buyer's
    /// copy of a tax invoice at its settle.
    Shown,
    /// The person's own copy, reprinted by `Actor`: the value prints only if they hold
    /// `data.personal.read`, and each subject printed writes an audit line.
    Reprinted(Actor),
}

/// One field's value as a document prints it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrintedValue {
    /// A personal value this document does not print: the label alone.
    Withheld,
    /// A whole number.
    Integer(i64),
    /// Yes or no.
    Boolean(bool),
    /// An entry of a list: its label's key.
    Entry(TranslationKey),
    /// A date.
    Date(CalendarDate),
    /// A quantity, with its unit's key.
    Quantity(Quantity, Option<TranslationKey>),
    /// A personal value, on the person's own copy.
    Personal(String),
}

/// One field a document prints.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintedField {
    /// The field's label key, which the paper prints in its language.
    pub label_key: TranslationKey,
    /// Its value.
    pub value: PrintedValue,
}

impl<S: EventStore + SubjectStore> Edge<S> {
    /// The sales channel a document's template is picked by: its order's, or
    /// `SALES_CHANNEL_UNSPECIFIED` (the base template) for a record, or an order whose channel this
    /// edge does not know.
    #[must_use]
    pub fn printed_channel(&self, objects: PrintedObjects) -> SalesChannel {
        self.printed_order(objects)
            .and_then(|order_id| self.lock_projection().order_channel(order_id))
            .unwrap_or(SalesChannel::Unspecified)
    }

    /// The fields `custom_field_ids` names that a document about `objects` prints, in that order.
    ///
    /// A field is printed from the object it is declared on among the document's (the bill and its
    /// order, the order, or the record), with the running package's declaration. One the package
    /// does not declare there, or that holds no value, is skipped. A personal value is printed as
    /// `personal` allows; the subject it comes from is read on this store and never logged.
    pub async fn printed_fields(
        &self,
        custom_field_ids: &[CustomFieldId],
        objects: PrintedObjects,
        personal: PersonalOnPaper,
    ) -> Vec<PrintedField> {
        let Some(package) = self.session().package.clone() else {
            return Vec::new();
        };
        let personal = self.personal_allowed(personal);
        let views = self.printed_views(objects);
        let mut printed = Vec::new();
        let mut audited: Vec<SubjectId> = Vec::new();
        for custom_field_id in custom_field_ids {
            let Some((field, view)) = views.iter().find_map(|(kind, view)| {
                declared(&package, *kind, *custom_field_id).zip(Some(view))
            }) else {
                continue;
            };
            let value = if field.personal == Some(true) {
                let value = self.personal_value(field, view, personal).await;
                // A reprint that prints a subject's value says so once per subject.
                if let (
                    Some(PrintedValue::Personal(_)),
                    PersonalOnPaper::Reprinted(actor),
                    Some(subject_id),
                ) = (&value, personal, view.subject_id)
                    && !audited.contains(&subject_id)
                {
                    audit_reprint(actor, subject_id);
                    audited.push(subject_id);
                }
                value
            } else {
                view.values
                    .iter()
                    .find(|value| value.custom_field_id == field.custom_field_id)
                    .and_then(|value| typed_value(&package, field, value))
            };
            if let Some(value) = value {
                printed.push(PrintedField {
                    label_key: field.label_key.clone(),
                    value,
                });
            }
        }
        printed
    }

    /// `personal` once the reprinting person's permission is known: a reprint by someone without
    /// `data.personal.read` withholds.
    fn personal_allowed(&self, personal: PersonalOnPaper) -> PersonalOnPaper {
        match personal {
            PersonalOnPaper::Reprinted(actor)
                if !self
                    .decision_ctx(actor)
                    .is_ok_and(|ctx| ctx.require(Permission::ReadPersonalData).is_ok()) =>
            {
                PersonalOnPaper::Withheld
            }
            allowed => allowed,
        }
    }

    /// A personal field's value for a document: withheld unless it may print, and none when the
    /// subject holds nothing for it, or has been masked.
    async fn personal_value(
        &self,
        field: &CustomField,
        view: &CustomDataView,
        allowed: PersonalOnPaper,
    ) -> Option<PrintedValue> {
        let subject_id = view.subject_id?;
        if allowed == PersonalOnPaper::Withheld {
            return Some(PrintedValue::Withheld);
        }
        let record = self
            .store
            .fetch(self.identity.store_id, subject_id)
            .await
            .ok()
            .flatten()
            .filter(|record| !record.is_masked())?;
        let text = record
            .fields
            .get(&field.custom_field_id.to_string())
            .filter(|text| !text.trim().is_empty())?
            .clone();
        Some(PrintedValue::Personal(text))
    }

    /// The order a document is about, if any.
    fn printed_order(&self, objects: PrintedObjects) -> Option<OrderId> {
        match objects {
            PrintedObjects::Bill(bill_id) => self
                .lock_projection()
                .bill(bill_id)
                .map(|bill| bill.order_id),
            PrintedObjects::Order(order_id) => Some(order_id),
            PrintedObjects::Record(_) => None,
        }
    }

    /// The custom data of each object a document is about, with the kind its fields are declared
    /// on: `None` for a record, whose fields are its type's.
    fn printed_views(&self, objects: PrintedObjects) -> Vec<(Option<DataObject>, CustomDataView)> {
        let object = |kind: DataObject, id: Ulid| {
            self.custom_object(kind, id).map(|view| (Some(kind), view))
        };
        match objects {
            PrintedObjects::Bill(bill_id) => [
                object(DataObject::Bill, bill_id.as_ulid()),
                self.printed_order(objects)
                    .and_then(|order_id| object(DataObject::Order, order_id.as_ulid())),
            ]
            .into_iter()
            .flatten()
            .collect(),
            PrintedObjects::Order(order_id) => object(DataObject::Order, order_id.as_ulid())
                .into_iter()
                .collect(),
            PrintedObjects::Record(custom_record_id) => self
                .custom_record(custom_record_id)
                .map(|view| (None, view))
                .into_iter()
                .collect(),
        }
    }
}

/// The declaration of `custom_field_id` on `kind`, or in the record's object type for `None`.
fn declared(
    package: &PackageContent,
    kind: Option<DataObject>,
    custom_field_id: CustomFieldId,
) -> Option<&CustomField> {
    let data = &package.data;
    match kind {
        Some(kind) => data
            .custom_fields
            .iter()
            .find(|field| field.custom_field_id == custom_field_id && field.object.known() == kind),
        None => data
            .object_types
            .iter()
            .flat_map(|object_type| object_type.fields.iter())
            .find(|field| field.custom_field_id == custom_field_id),
    }
}

/// A non-personal value as its field's type prints it, or `None` when it holds none of that type.
fn typed_value(
    package: &PackageContent,
    field: &CustomField,
    value: &CustomFieldValue,
) -> Option<PrintedValue> {
    match field.field_type.known() {
        CustomFieldType::Int => value.integer.map(PrintedValue::Integer),
        CustomFieldType::Bool => value.boolean.map(PrintedValue::Boolean),
        CustomFieldType::Date => value.calendar_date.map(PrintedValue::Date),
        CustomFieldType::Quantity => value
            .quantity
            .map(|quantity| PrintedValue::Quantity(quantity, field.unit_key.clone())),
        CustomFieldType::Choice => {
            let entry_id = value.list_entry_id?;
            package
                .data
                .lists
                .iter()
                .flat_map(|list| list.entries.iter())
                .find(|entry| entry.list_entry_id == entry_id)
                .map(|entry| PrintedValue::Entry(entry.label_key.clone()))
        }
        CustomFieldType::Text | CustomFieldType::Unspecified => None,
    }
}

/// The audit line a reprint that prints a personal value writes (ADR-0206 decision 5): the person,
/// the device and the subject, never a value. In the store's own log only.
fn audit_reprint(actor: Actor, subject_id: SubjectId) {
    tracing::info!(
        target: "audit",
        action = "data.subject.printed",
        employee_id = %actor.employee_id,
        device_id = %actor.device_id,
        subject_id = %subject_id,
        "a subject's values were printed on a reprint"
    );
}

#[cfg(test)]
mod tests;
