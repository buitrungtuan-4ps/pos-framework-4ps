// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The custom-data report: how many objects hold each value of one field
//! ([ADR-0194](../../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)
//! decision 7).
//!
//! A report groups and filters by non-personal fields only. The route refuses a personal one, and
//! the projection holds no personal value anyway. Nothing here reads, groups or returns a
//! `subject_id`: a count of objects per value is all a report answers.

use core::future::Future;

use serde::Serialize;

use pos_cloud_api::csv_file::finish;
use pos_ports::PortError;
use pos_proto::events::CustomFieldValue;
use pos_proto::ids::{CustomFieldId, ListEntryId, ObjectTypeId, StoreId, TenantId};
use pos_proto::package::DataObject;

/// The most groups a summary returns. A list has at most 500 entries, so a choice field always fits;
/// an integer or date field with more distinct values is cut, and says so.
pub const SUMMARY_GROUPS_MAX: usize = 500;

/// What the summary counts: the objects of one kind, or the records of one object type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SummaryObject {
    /// An existing object's kind.
    Existing(DataObject),
    /// A package's object type.
    Record(ObjectTypeId),
}

/// A value a filter field must hold.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterValue {
    /// A list entry, for a `choice` field.
    ListEntry(ListEntryId),
    /// Yes or no, for a `bool` field.
    Boolean(bool),
    /// A whole number, for an `int` field.
    Integer(i64),
}

/// One summary: group the objects by `custom_field_id`, among those whose `filter` field holds its
/// value, if one is given.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SummaryQuery {
    /// What is counted.
    pub object: SummaryObject,
    /// The field the objects are grouped by.
    pub custom_field_id: CustomFieldId,
    /// A field and the value it must hold.
    pub filter: Option<(CustomFieldId, FilterValue)>,
}

/// How many objects hold one value.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct ValueCount {
    /// The value, with the field it is a value of.
    pub value: CustomFieldValue,
    /// How many objects hold it.
    pub objects: u64,
}

/// A summary's answer.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct ValueSummary {
    /// The groups, the most objects first, at most [`SUMMARY_GROUPS_MAX`].
    pub groups: Vec<ValueCount>,
    /// Whether more groups exist than were returned.
    pub truncated: bool,
}

/// Answers a summary from a store's projected custom values.
pub trait CustomValueSummary {
    /// The groups for `query` in one store of one tenant, the most objects first, at most `limit`.
    fn summary(
        &self,
        tenant: TenantId,
        store_id: StoreId,
        query: &SummaryQuery,
        limit: usize,
    ) -> impl Future<Output = Result<Vec<ValueCount>, PortError>> + Send;
}

/// Runs a summary, asking for one group more than it returns so it can say whether it cut any.
///
/// # Errors
///
/// [`PortError`] if the projection cannot be read.
pub async fn summarise<S: CustomValueSummary>(
    values: &S,
    tenant: TenantId,
    store_id: StoreId,
    query: &SummaryQuery,
) -> Result<ValueSummary, PortError> {
    let mut groups = values
        .summary(
            tenant,
            store_id,
            query,
            SUMMARY_GROUPS_MAX.saturating_add(1),
        )
        .await?;
    let truncated = groups.len() > SUMMARY_GROUPS_MAX;
    groups.truncate(SUMMARY_GROUPS_MAX);
    Ok(ValueSummary { groups, truncated })
}

/// A summary as CSV: one row per group, its value in the column of its type and the count.
///
/// # Errors
///
/// A `csv::Error` if serialisation fails.
pub fn summary_csv(summary: &ValueSummary) -> Result<Vec<u8>, csv::Error> {
    let mut writer = csv::Writer::from_writer(Vec::new());
    writer.write_record([
        "custom_field_id",
        "list_entry_id",
        "boolean",
        "integer",
        "calendar_date",
        "quantity_milli",
        "objects",
    ])?;
    for group in &summary.groups {
        let value = &group.value;
        writer.write_record([
            value.custom_field_id.to_string(),
            value
                .list_entry_id
                .map(|id| id.to_string())
                .unwrap_or_default(),
            value.boolean.map(|b| b.to_string()).unwrap_or_default(),
            value.integer.map(|n| n.to_string()).unwrap_or_default(),
            value
                .calendar_date
                .map(|d| d.to_string())
                .unwrap_or_default(),
            value
                .quantity
                .map(|q| q.milli.to_string())
                .unwrap_or_default(),
            group.objects.to_string(),
        ])?;
    }
    finish(writer)
}
