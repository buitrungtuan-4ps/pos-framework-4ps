// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The custom-data report: how many objects of a store hold each value of one field
//! ([ADR-0194](../../../../docs/adr/0194-custom-data-is-declared-and-personal-fields-are-kept-apart.md)
//! decision 7).
//!
//! A report groups and filters by non-personal fields only. The route refuses `PERSONAL_FIELD` for
//! a field any version of the tenant's packages declares personal, and it never reads, groups or
//! returns a `subject_id`.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde::Deserialize;

use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error, csv_download_response, parse_ulid_fields};
use pos_cloud_config::packages::PackageStore;
use pos_cloud_reports::custom_data::{
    CustomValueSummary, FilterValue, SummaryObject, SummaryQuery, ValueSummary, summarise,
    summary_csv,
};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{CustomFieldId, ObjectTypeId, StoreId, TenantId};
use pos_proto::package::{CustomField, CustomFieldType, DataObject};
use pos_proto::wire_enum::WireEnum;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{FieldRefusal, Json};

/// The state the report routes share.
#[derive(Clone)]
pub(crate) struct ReportState<V, P, A, C> {
    values: V,
    packages: P,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A summary's query string.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
pub(crate) struct SummaryParams {
    /// The tenant the store belongs to (a 26-character ULID).
    tenant_id: String,
    /// The field to group by (a 26-character ULID).
    custom_field_id: String,
    /// The existing objects to count, as `DATA_OBJECT_ORDER` and the like. Give this or
    /// `object_type_id`.
    object: Option<String>,
    /// The object type whose records to count. Give this or `object`.
    object_type_id: Option<String>,
    /// A field the counted objects must hold `filter_value` in.
    filter_field_id: Option<String>,
    /// The value: a list entry's id, `true` or `false`, or a whole number, as the filter field's
    /// type is.
    filter_value: Option<String>,
}

/// Builds the custom-data report router, behind `console.data.read`.
pub fn custom_value_report_router<V, P, A, C>(
    values: V,
    packages: P,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    V: CustomValueSummary + Clone + Send + Sync + 'static,
    P: PackageStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/stores/{store_id}/custom-values/summary",
            get(admin_custom_value_summary::<V, P, A, C>),
        )
        .route(
            "/admin/stores/{store_id}/custom-values/summary.csv",
            get(admin_custom_value_summary_csv::<V, P, A, C>),
        )
        .with_state(ReportState {
            values,
            packages,
            admin,
            clock,
            audit,
        })
}

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/custom-values/summary",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    params(("store_id" = String, Path, description = "The store (a 26-character ULID)"), SummaryParams),
    responses(
        (status = 200, body = ValueSummary, description = "How many objects hold each value of the \
                                      field, the most first, at most 500 groups; `truncated` says \
                                      whether there were more. No person is named or counted"),
        (status = 400, description = "An id is not a ULID; neither or both of `object` and \
                                      `object_type_id` are given; a filter has no value; the field \
                                      is not declared (`UNKNOWN`), is declared personal \
                                      (`PERSONAL_FIELD`), or cannot filter (`INVALID`)", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The projection or the package store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "reports",
)]
/// A console user reads how many of a store's objects hold each value of one custom field.
pub(crate) async fn admin_custom_value_summary<V, P, A, C>(
    State(state): State<ReportState<V, P, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(params): Query<SummaryParams>,
) -> Response
where
    V: CustomValueSummary + Clone + Send + Sync + 'static,
    P: PackageStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    match answer(&state, &headers, &store_id, &params).await {
        Ok((_, _, summary)) => (StatusCode::OK, Json(summary)).into_response(),
        Err(refusal) => refusal,
    }
}

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/custom-values/summary.csv",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    params(("store_id" = String, Path, description = "The store (a 26-character ULID)"), SummaryParams),
    responses(
        (status = 200, body = String, content_type = "text/csv", description = "`custom-values.csv`: \
                                      `custom_field_id`, the value in the column of its type \
                                      (`list_entry_id`, `boolean`, `integer`, `calendar_date`, \
                                      `quantity_milli`) and `objects`. The export is audited by \
                                      ids and row count"),
        (status = 400, description = "As the summary read", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The projection or the package store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "reports",
)]
/// A console user downloads the summary as CSV.
pub(crate) async fn admin_custom_value_summary_csv<V, P, A, C>(
    State(state): State<ReportState<V, P, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(params): Query<SummaryParams>,
) -> Response
where
    V: CustomValueSummary + Clone + Send + Sync + 'static,
    P: PackageStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let (actor, tenant, summary) = match answer(&state, &headers, &store_id, &params).await {
        Ok(answered) => answered,
        Err(refusal) => return refusal,
    };
    let Ok(body) = summary_csv(&summary) else {
        return unavailable();
    };
    audit_action(
        &state.audit,
        &state.clock,
        &actor,
        Some(tenant),
        "custom_values.summary.export",
        "store",
        &store_id,
        None,
        Some(serde_json::json!({
            "custom_field_id": params.custom_field_id,
            "rows": summary.groups.len(),
        })),
    )
    .await;
    csv_download_response("custom-values.csv", body)
}

/// Checks the caller, the ids and the fields, and runs the summary.
async fn answer<V, P, A, C>(
    state: &ReportState<V, P, A, C>,
    headers: &HeaderMap,
    store_id: &str,
    params: &SummaryParams,
) -> Result<(crate::auth::admin::AdminContext, TenantId, ValueSummary), Response>
where
    V: CustomValueSummary + Clone + Send + Sync + 'static,
    P: PackageStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let actor =
        require_permission(&state.admin, &state.clock, headers, ConsolePermission::Read).await?;
    let [tenant, store, field] = parse_ulid_fields([
        ("tenant_id", params.tenant_id.as_str()),
        ("store_id", store_id),
        ("custom_field_id", params.custom_field_id.as_str()),
    ])?;
    let tenant = TenantId::new(tenant);
    let object = summary_object(params)?;
    let declared = declarations(&state.packages, tenant).await?;
    let custom_field_id = CustomFieldId::new(field);
    reportable(&declared, custom_field_id, "custom_field_id")?;
    let filter = match (&params.filter_field_id, &params.filter_value) {
        (None, None) => None,
        (Some(field), Some(value)) => {
            let [field] = parse_ulid_fields([("filter_field_id", field.as_str())])?;
            let field = CustomFieldId::new(field);
            let field_type = reportable(&declared, field, "filter_field_id")?;
            Some((field, filter_value(field_type, value)?))
        }
        _ => {
            return Err(FieldRefusal::new(
                "a filter names a field and a value",
                "filter_value",
                "REQUIRED",
            )
            .into_response());
        }
    };
    let query = SummaryQuery {
        object,
        custom_field_id,
        filter,
    };
    let summary = summarise(&state.values, tenant, StoreId::new(store), &query)
        .await
        .map_err(|error| {
            tracing::error!(%error, "reading the custom-value projection failed");
            unavailable()
        })?;
    Ok((actor, tenant, summary))
}

/// What the summary counts: exactly one of `object` and `object_type_id`.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the shared 400 these route helpers return"
)]
fn summary_object(params: &SummaryParams) -> Result<SummaryObject, Response> {
    match (&params.object, &params.object_type_id) {
        (Some(token), None) => DataObject::ALL
            .iter()
            .find(|kind| **kind != DataObject::Unspecified && kind.as_wire() == token)
            .map(|kind| SummaryObject::Existing(*kind))
            .ok_or_else(|| {
                FieldRefusal::new("an object is a DATA_OBJECT_ token", "object", "INVALID")
                    .into_response()
            }),
        (None, Some(id)) => {
            let [id] = parse_ulid_fields([("object_type_id", id.as_str())])?;
            Ok(SummaryObject::Record(ObjectTypeId::new(id)))
        }
        _ => Err(FieldRefusal::new(
            "a summary counts an object or an object type, one of the two",
            "object",
            "REQUIRED",
        )
        .into_response()),
    }
}

/// Every field every version of the tenant's packages declares.
async fn declarations<P: PackageStore>(
    packages: &P,
    tenant: TenantId,
) -> Result<Vec<CustomField>, Response> {
    let failed = |error: pos_cloud_config::packages::PackageStoreError| {
        tracing::error!(%error, "reading the package store for a report failed");
        unavailable()
    };
    let mut fields = Vec::new();
    for package in packages.list_packages(tenant).await.map_err(failed)? {
        for version in packages
            .list_versions(tenant, package.package_id)
            .await
            .map_err(failed)?
        {
            let data = version.content.data;
            fields.extend(data.custom_fields);
            fields.extend(data.object_types.into_iter().flat_map(|kind| kind.fields));
        }
    }
    Ok(fields)
}

/// Refuses a field no version declares, or one any version declares personal. Returns its type.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the shared 400 these route helpers return"
)]
fn reportable(
    declared: &[CustomField],
    field: CustomFieldId,
    name: &'static str,
) -> Result<CustomFieldType, Response> {
    let versions: Vec<&CustomField> = declared
        .iter()
        .filter(|declaration| declaration.custom_field_id == field)
        .collect();
    let Some(first) = versions.first() else {
        return Err(
            FieldRefusal::new("no package declares this field", name, "UNKNOWN").into_response(),
        );
    };
    if versions
        .iter()
        .any(|declaration| declaration.personal != Some(false))
    {
        return Err(FieldRefusal::new(
            "a report never groups or filters by a personal field",
            name,
            "PERSONAL_FIELD",
        )
        .into_response());
    }
    Ok(first.field_type.known())
}

/// A filter value read as the filter field's type: a list entry, yes or no, or a whole number.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the shared 400 these route helpers return"
)]
fn filter_value(field_type: CustomFieldType, value: &str) -> Result<FilterValue, Response> {
    let refused = || {
        FieldRefusal::new(
            "a filter value is a list entry's id, true or false, or a whole number, as its field is",
            "filter_value",
            "INVALID",
        )
        .into_response()
    };
    match field_type {
        CustomFieldType::Choice => value
            .parse()
            .map(FilterValue::ListEntry)
            .map_err(|_| refused()),
        CustomFieldType::Bool => value
            .parse()
            .map(FilterValue::Boolean)
            .map_err(|_| refused()),
        CustomFieldType::Int => value
            .parse()
            .map(FilterValue::Integer)
            .map_err(|_| refused()),
        CustomFieldType::Unspecified
        | CustomFieldType::Date
        | CustomFieldType::Quantity
        | CustomFieldType::Text => Err(refused()),
    }
}

/// The `503` a report earns when what it reads is unreachable.
fn unavailable() -> Response {
    api_error(ErrorStatus::Unavailable, "the report is unavailable")
}
