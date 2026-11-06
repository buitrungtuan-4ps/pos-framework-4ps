// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Custom data at the till
//! ([ADR-0194](../../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md),
//! [ADR-0206](../../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)):
//! custom fields on an existing object, records of a package's object types, and the read of one
//! subject's personal values.
//!
//! A write takes each value typed, and a `text` value as typed. No response of these routes but
//! `GET /api/subjects/{subject_id}` carries a personal value: a record or object answers with its
//! non-personal values and the `subject_id` of its personal ones, and showing those again needs
//! `data.personal.read` (decision 7). A request's `Debug` never shows a text value, so no log line
//! can carry one.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::Json;
use axum::extract::rejection::QueryRejection;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use pos_core::decision::Actor;
use pos_core::package::{EnteredValue, Entry, PersonalText};
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;
use pos_proto::events::CustomFieldValue;
use pos_proto::ids::{
    CustomFieldId, CustomRecordId, ListEntryId, ObjectTypeId, PackageVersionId, SubjectId,
};
use pos_proto::package::DataObject;
use pos_proto::quantity::Quantity;
use pos_proto::text::TranslationKey;
use pos_proto::time::{CalendarDate, Timestamp};
use pos_proto::ulid::Ulid;
use pos_proto::{Open, WireEnum};

use crate::app::{AppError, CustomDataView, Edge};
use crate::http::{bad_request, error_response, parse_ulid};

/// The most records `GET /api/custom-records` returns.
const RECORDS_LISTED_MAX: usize = 200;

/// One value as the till sends it: at most one of the value fields, the one the field's type
/// declares; none clears the value.
#[derive(Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct ValueRequest {
    custom_field_id: CustomFieldId,
    #[serde(default)]
    integer: Option<i64>,
    #[serde(default)]
    boolean: Option<bool>,
    #[serde(default)]
    list_entry_id: Option<ListEntryId>,
    #[serde(default)]
    calendar_date: Option<CalendarDate>,
    #[serde(default)]
    quantity: Option<Quantity>,
    /// A `text` field's value, at most 1 KiB. Always personal: kept on this store only, and never
    /// logged.
    #[serde(default)]
    text: Option<String>,
}

impl std::fmt::Debug for ValueRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ValueRequest")
            .field("custom_field_id", &self.custom_field_id)
            .finish_non_exhaustive()
    }
}

impl ValueRequest {
    /// The entry, or `None` when more than one value field is present.
    fn entry(self) -> Option<Entry> {
        let mut given = Vec::new();
        if let Some(integer) = self.integer {
            given.push(EnteredValue::Integer(integer));
        }
        if let Some(boolean) = self.boolean {
            given.push(EnteredValue::Boolean(boolean));
        }
        if let Some(entry) = self.list_entry_id {
            given.push(EnteredValue::ListEntry(entry));
        }
        if let Some(date) = self.calendar_date {
            given.push(EnteredValue::Date(date));
        }
        if let Some(quantity) = self.quantity {
            given.push(EnteredValue::Quantity(quantity));
        }
        if let Some(text) = self.text {
            given.push(EnteredValue::Text(PersonalText::new(text)));
        }
        let value = match given.len() {
            0 => EnteredValue::Cleared,
            1 => given.pop()?,
            _ => return None,
        };
        Some(Entry {
            custom_field_id: self.custom_field_id,
            value,
        })
    }
}

/// The entries of one write, or `None` when a value names more than one value field.
fn entries(values: Vec<ValueRequest>) -> Option<Vec<Entry>> {
    values.into_iter().map(ValueRequest::entry).collect()
}

/// A new record: its object type, and its values.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct NewRecordRequest {
    object_type_id: ObjectTypeId,
    /// The values that changed.
    #[serde(default)]
    values: Vec<ValueRequest>,
    /// The consent purposes the person agreed to on this screen, by their label keys. A personal
    /// value needs its field's (ADR-0194 decision 5).
    #[serde(default)]
    consented_purpose_keys: Vec<TranslationKey>,
}

/// Custom fields set on an existing object.
#[derive(Debug, Deserialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct FieldsRequest {
    /// `DATA_OBJECT_ORDER`, `_LINE`, `_BILL`, `_TABLE` or `_SHIFT`.
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    object: Open<DataObject>,
    /// The object's id.
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    object_id: Ulid,
    /// The values that changed.
    #[serde(default)]
    values: Vec<ValueRequest>,
    /// The consent purposes the person agreed to on this screen, by their label keys.
    #[serde(default)]
    consented_purpose_keys: Vec<TranslationKey>,
}

/// Which object's custom fields to read.
#[derive(Debug, Deserialize)]
pub(crate) struct ObjectQuery {
    object: String,
    object_id: String,
}

/// Which object type's records to list.
#[derive(Debug, Deserialize)]
pub(crate) struct RecordsQuery {
    object_type_id: String,
}

/// An object's or a record's custom data, without any personal value.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct CustomDataResponse {
    /// The record, for a custom record.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    custom_record_id: Option<CustomRecordId>,
    /// The record's object type, for a custom record.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    object_type_id: Option<ObjectTypeId>,
    /// The non-personal values, by field.
    values: Vec<CustomFieldValue>,
    /// The subject holding the personal values, once there are any. Reading them needs
    /// `data.personal.read`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(nullable = false))]
    subject_id: Option<SubjectId>,
    /// The package version the last change followed.
    package_version_id: PackageVersionId,
}

impl From<CustomDataView> for CustomDataResponse {
    fn from(view: CustomDataView) -> Self {
        Self {
            custom_record_id: view.custom_record_id,
            object_type_id: view.object_type_id,
            values: view.values,
            subject_id: view.subject_id,
            package_version_id: view.package_version_id,
        }
    }
}

/// The records of one object type.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct CustomRecordsResponse {
    /// Newest first, at most 200.
    records: Vec<CustomDataResponse>,
}

/// One subject's record (ADR-0206 decision 5): its personal values by field, and when each consent
/// was given, as `consent.<purpose label key>`.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "openapi", derive(utoipa::ToSchema))]
pub(crate) struct SubjectResponse {
    subject_id: SubjectId,
    /// When the data was collected.
    #[cfg_attr(feature = "openapi", schema(value_type = String))]
    collected_time: Timestamp,
    /// When it expires, where it has its own period.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, nullable = false))]
    retain_until_time: Option<Timestamp>,
    /// When it was masked; every value then reads `[REDACTED]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(feature = "openapi", schema(value_type = String, nullable = false))]
    masked_time: Option<Timestamp>,
    /// The values by field id, and the consent times.
    fields: BTreeMap<String, String>,
}

fn answer(result: Result<CustomDataView, AppError>) -> Response {
    match result {
        Ok(view) => (StatusCode::OK, Json(CustomDataResponse::from(view))).into_response(),
        Err(error) => error_response(&error),
    }
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/custom-fields",
    security(("till_session" = [])),
    extensions(("x-permission" = json!(["data.record.write", "sales.line.add", "billing.bill.open", "sales.table.manage", "cash.shift.open"]))),
    request_body(content = FieldsRequest, content_type = "application/json",
        description = "The object, its id, the values that changed and the consents given. A `text` value is \
                       personal and never logged"),
    responses(
        (status = 200, body = CustomDataResponse, description = "Set: the object's non-personal values and its subject"),
        (status = 400, description = "The body is malformed, or a value names more than one value field"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; or this box \
                                      was superseded by a replacement"),
        (status = 409, description = "Refused: the person lacks `data.record.write` or the object's own permission \
                                      (`PERMISSION_DENIED`), or the edge does not know the object (`UNKNOWN_OBJECT`)"),
        (status = 422, description = "A value does not fit the package's declarations, or a personal value \
                                      lacks its consent (`CUSTOM_ENTRY_REFUSED`); the message names the field, \
                                      never the value"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
    ),
    tag = "custom data",
))]
/// `POST /api/custom-fields` — sets custom fields on an existing object (ADR-0194, ADR-0206).
pub(crate) async fn set_custom_fields<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<FieldsRequest>,
) -> Response
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    let Ok(object) = request.object.require() else {
        return bad_request("object is not a known DATA_OBJECT");
    };
    let object_id = request.object_id;
    let consented = request.consented_purpose_keys;
    let Some(entries) = entries(request.values) else {
        return bad_request("a value names more than one value field");
    };
    answer(
        edge.set_custom_fields(actor, object, object_id, &entries, &consented)
            .await,
    )
}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/custom-fields",
    security(("till_session" = [])),
    params(
        ("object" = String, Query, description = "`DATA_OBJECT_ORDER`, `_LINE`, `_BILL`, `_TABLE` or `_SHIFT`"),
        ("object_id" = String, Query, description = "The object's id, a ULID"),
    ),
    responses(
        (status = 200, body = CustomDataResponse, description = "The object's non-personal values and its subject"),
        (status = 400, description = "The object or its id is malformed"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed"),
        (status = 409, description = "The object has no custom data on this edge (`UNKNOWN_OBJECT`)"),
    ),
    tag = "custom data",
))]
/// `GET /api/custom-fields` — one object's custom data.
pub(crate) async fn object_custom_fields<S>(
    State(edge): State<Arc<Edge<S>>>,
    query: Result<Query<ObjectQuery>, QueryRejection>,
) -> Response
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    let Ok(Query(query)) = query else {
        return bad_request("object and object_id are required");
    };
    let Some(object) = DataObject::from_wire(&query.object) else {
        return bad_request("object is not a known DATA_OBJECT");
    };
    let Some(object_id) = parse_ulid(&query.object_id) else {
        return bad_request("object_id is not a ULID");
    };
    answer(
        edge.custom_object(object, object_id)
            .ok_or(AppError::UnknownObject),
    )
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/custom-records",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("data.record.write"))),
    request_body(content = NewRecordRequest, content_type = "application/json",
        description = "The object type, its values and the consents given. A `text` value is personal and \
                       never logged"),
    responses(
        (status = 200, body = CustomDataResponse, description = "Created: the record's id, its non-personal values and its subject"),
        (status = 400, description = "The body is malformed, or a value names more than one value field"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; or this box \
                                      was superseded by a replacement"),
        (status = 409, description = "Refused: the person lacks `data.record.write` (`PERMISSION_DENIED`)"),
        (status = 422, description = "The package declares no such type, a value does not fit, a required \
                                      field is missing, or a personal value lacks its consent \
                                      (`CUSTOM_ENTRY_REFUSED`); the message names the field, never the value"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
    ),
    tag = "custom data",
))]
/// `POST /api/custom-records` — creates a record of a package's object type.
pub(crate) async fn create_custom_record<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Json(request): Json<NewRecordRequest>,
) -> Response
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    let object_type_id = request.object_type_id;
    let consented = request.consented_purpose_keys;
    let Some(entries) = entries(request.values) else {
        return bad_request("a value names more than one value field");
    };
    answer(
        edge.save_custom_record(actor, object_type_id, None, &entries, &consented)
            .await,
    )
}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/custom-records",
    security(("till_session" = [])),
    params(("object_type_id" = String, Query, description = "The object type, a ULID")),
    responses(
        (status = 200, body = CustomRecordsResponse, description = "The type's records this edge holds, newest first, \
                                      at most 200, without any personal value"),
        (status = 400, description = "The object type is not a ULID"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed"),
    ),
    tag = "custom data",
))]
/// `GET /api/custom-records` — the records of one object type.
pub(crate) async fn list_custom_records<S>(
    State(edge): State<Arc<Edge<S>>>,
    query: Result<Query<RecordsQuery>, QueryRejection>,
) -> Response
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    let Some(object_type_id) = query
        .ok()
        .and_then(|Query(query)| parse_ulid(&query.object_type_id))
    else {
        return bad_request("object_type_id is not a ULID");
    };
    let records = edge
        .custom_records(ObjectTypeId::new(object_type_id), RECORDS_LISTED_MAX)
        .into_iter()
        .map(CustomDataResponse::from)
        .collect();
    (StatusCode::OK, Json(CustomRecordsResponse { records })).into_response()
}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/custom-records/{id}",
    security(("till_session" = [])),
    params(("id" = String, Path, description = "The record's id, a ULID")),
    responses(
        (status = 200, body = CustomDataResponse, description = "The record's non-personal values and its subject"),
        (status = 400, description = "The id is not a ULID"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed"),
        (status = 409, description = "This edge holds no such record (`UNKNOWN_CUSTOM_RECORD`)"),
    ),
    tag = "custom data",
))]
/// `GET /api/custom-records/{id}` — one record.
pub(crate) async fn custom_record<S>(
    State(edge): State<Arc<Edge<S>>>,
    Path(id): Path<String>,
) -> Response
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    let Some(id) = parse_ulid(&id) else {
        return bad_request("the record id is not a ULID");
    };
    answer(
        edge.custom_record(CustomRecordId::new(id))
            .ok_or(AppError::UnknownCustomRecord),
    )
}

#[cfg_attr(feature = "openapi", utoipa::path(
    post,
    path = "/api/custom-records/{id}/values",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("data.record.write"))),
    params(("id" = String, Path, description = "The record's id, a ULID")),
    request_body(content = NewRecordRequest, content_type = "application/json",
        description = "The record's object type, the values that changed and the consents given. A `text` \
                       value is personal and never logged"),
    responses(
        (status = 200, body = CustomDataResponse, description = "Changed: the record's non-personal values and its subject"),
        (status = 400, description = "The id is not a ULID, the body is malformed, or a value names more than \
                                      one value field"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed; or this box \
                                      was superseded by a replacement"),
        (status = 409, description = "Refused: the person lacks `data.record.write` (`PERMISSION_DENIED`), or \
                                      this edge holds no such record of that type (`UNKNOWN_CUSTOM_RECORD`)"),
        (status = 422, description = "A value does not fit, a required field is cleared, or a personal value \
                                      lacks its consent (`CUSTOM_ENTRY_REFUSED`)"),
        (status = 503, description = "The store is unavailable, and nothing was recorded"),
    ),
    tag = "custom data",
))]
/// `POST /api/custom-records/{id}/values` — changes a record's values.
pub(crate) async fn change_custom_record<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(id): Path<String>,
    Json(request): Json<NewRecordRequest>,
) -> Response
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    let Some(id) = parse_ulid(&id) else {
        return bad_request("the record id is not a ULID");
    };
    let object_type_id = request.object_type_id;
    let consented = request.consented_purpose_keys;
    let Some(entries) = entries(request.values) else {
        return bad_request("a value names more than one value field");
    };
    answer(
        edge.save_custom_record(
            actor,
            object_type_id,
            Some(CustomRecordId::new(id)),
            &entries,
            &consented,
        )
        .await,
    )
}

#[cfg_attr(feature = "openapi", utoipa::path(
    get,
    path = "/api/subjects/{subject_id}",
    security(("till_session" = [])),
    extensions(("x-permission" = json!("data.personal.read"))),
    params(("subject_id" = String, Path, description = "The subject's id, a ULID")),
    responses(
        (status = 200, body = SubjectResponse, description = "The one subject's record. Every read is audited \
                                      by actor, subject and action, never by value"),
        (status = 400, description = "The id is not a ULID"),
        (status = 401, description = "This device is not paired: no token, or one the edge does not accept"),
        (status = 403, description = "Nobody is signed in on this device, or the sign-in lapsed"),
        (status = 409, description = "Refused: the person lacks `data.personal.read` (`PERMISSION_DENIED`), or \
                                      this store holds no such subject (`UNKNOWN_SUBJECT`)"),
        (status = 503, description = "The store is unavailable"),
    ),
    tag = "custom data",
))]
/// `GET /api/subjects/{subject_id}` — one subject's personal values (ADR-0206 decision 5). There is
/// no list and no search.
pub(crate) async fn read_subject<S>(
    State(edge): State<Arc<Edge<S>>>,
    Extension(actor): Extension<Actor>,
    Path(subject_id): Path<String>,
) -> Response
where
    S: EventStore + SubjectStore + Send + Sync + 'static,
{
    let Some(subject_id) = parse_ulid(&subject_id).map(SubjectId::new) else {
        return bad_request("the subject id is not a ULID");
    };
    match edge.read_subject(actor, subject_id).await {
        Ok(Some(record)) => (
            StatusCode::OK,
            Json(SubjectResponse {
                subject_id,
                collected_time: record.collected_at,
                retain_until_time: record.retain_until,
                masked_time: record.masked_at,
                fields: record.fields,
            }),
        )
            .into_response(),
        Ok(None) => error_response(&AppError::UnknownSubject),
        Err(error) => error_response(&error),
    }
}

#[cfg(test)]
mod tests {
    use super::ValueRequest;

    #[test]
    fn a_text_value_never_shows_in_debug() {
        let request: ValueRequest = serde_json::from_str(
            r#"{"custom_field_id":"01J0000000000000000000CF03","text":"demo note"}"#,
        )
        .expect("parses");
        assert!(!format!("{request:?}").contains("demo"));
    }

    #[test]
    fn a_value_names_at_most_one_value_field() {
        let two: ValueRequest = serde_json::from_str(
            r#"{"custom_field_id":"01J0000000000000000000CF03","integer":1,"boolean":true}"#,
        )
        .expect("parses");
        assert!(two.entry().is_none());
        let none: ValueRequest =
            serde_json::from_str(r#"{"custom_field_id":"01J0000000000000000000CF03"}"#)
                .expect("parses");
        assert!(none.entry().is_some(), "no value clears the field");
    }
}
