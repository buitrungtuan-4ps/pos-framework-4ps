// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Tax rates (`/admin/catalog/tax-rates`): the rate the edge applies per tax class and channel,
//! and its components ([ADR-0074](../../../../docs/adr/0074-localization-and-tax.md)).

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error, api_error_with_details, parse_ulid_fields};
use pos_proto::determinism::ClockSource;
use pos_proto::enums::SalesChannel;
use pos_proto::ids::{TaxClassId, TenantId};
use pos_proto::locale::{TaxComponent, TaxComponentName, TaxRate};
use pos_proto::{ErrorStatus, WireEnum};
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::CatalogStore;
use crate::http::catalog::catalog_error_response;
use crate::http::{
    RegistryTenantQuery, if_match_collection, not_found, version_mismatch, with_etag,
};
use crate::tax::{TaxRateEntry, TaxRateStore, TaxRateStoreError};
use crate::version::UpdateOutcome;

// --- Tax rates (ADR-0074, Track M4): the per-(tax class × channel) rate the edge applies ----------

/// The highest rate the table accepts, in basis points (100%). Matches the migration's CHECK.
const MAX_TAX_RATE_BPS: u32 = 10_000;

/// The state the tax-rate routes share: the rate store they write, the catalog store they validate a
/// class against, and the admin/clock/audit every `/admin` write needs.
#[derive(Clone)]
struct TaxRateState<Tax, Cat, A, C> {
    tax_rates: Tax,
    catalog: Cat,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// One authored tax-rate row on the wire: the class id, the channel's wire token, the rate in basis
/// points (10% is `1000`), and how that rate is broken out on the invoice.
#[derive(Debug, Clone, serde::Serialize)]
struct TaxRateView {
    tax_class_id: String,
    sales_channel: String,
    rate_bps: u32,
    /// The named parts the invoice prints, which must sum to `rate_bps`
    /// ([ADR-0104](../../../../docs/adr/0104-multi-component-and-inclusive-tax.md)). Empty for a rate
    /// printed as one line, which is most of the world.
    components: Vec<TaxComponentView>,
    /// `true` when one of `components` is named with anything but a [`TaxComponentName`]: two to
    /// eight upper-case letters and digits, starting with a letter
    /// ([ADR-0168](../../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 2).
    ///
    /// Only a row saved before the grid checked names can be so. It is kept as it was saved, and
    /// its stores charge its rate without a breakdown. A save replaces the whole grid and refuses
    /// such a name, so the next save is refused until it is changed, which is what the console
    /// flags. Absent otherwise, so a sound table reads as it always did. Output only: a save
    /// reads no such field.
    #[serde(skip_serializing_if = "core::ops::Not::not")]
    component_name_invalid: bool,
}

/// One named part of a rate, on the wire.
#[derive(Debug, Clone, serde::Serialize, Deserialize)]
struct TaxComponentView {
    /// What the invoice calls it — `CGST`, `SGST`, `IGST`.
    name: String,
    /// This part's share, in basis points.
    rate_bps: u32,
}

/// A `PUT /admin/catalog/tax-rates` body: the tenant and its whole rate table (a wholesale replace).
#[derive(Debug, Clone, Deserialize)]
struct SetTaxRatesRequest {
    tenant_id: String,
    rates: Vec<TaxRateRowRequest>,
}

/// One row of a [`SetTaxRatesRequest`].
#[derive(Debug, Clone, Deserialize)]
struct TaxRateRowRequest {
    tax_class_id: String,
    sales_channel: String,
    rate_bps: u32,
    /// How the rate is broken out on the invoice. `#[serde(default)]`, so a console that predates
    /// ADR-0104 still saves a grid and every row keeps printing as one line.
    #[serde(default)]
    components: Vec<TaxComponentView>,
}

/// Builds the tax-rate sub-router ([ADR-0074](../../../../docs/adr/0074-localization-and-tax.md), M4).
///
/// One resource: the tenant's per-(tax class × channel) rate table. `GET` lists it (behind
/// [`ConsolePermission::Read`]); `PUT` replaces it wholesale (behind
/// [`ConsolePermission::ManageCatalog`], the permission tax *classes* already use), validating each
/// row's class, channel, and rate before the write, and auditing `tax_rate.set`.
pub fn tax_rate_router<Tax, Cat, A, C>(
    tax_rates: Tax,
    catalog: Cat,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Tax: TaxRateStore + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/catalog/tax-rates",
            get(admin_list_tax_rates::<Tax, Cat, A, C>).put(admin_set_tax_rates::<Tax, Cat, A, C>),
        )
        .with_state(TaxRateState {
            tax_rates,
            catalog,
            admin,
            clock,
            audit,
        })
}

/// One authored entry as the wire view.
fn tax_rate_view(entry: &TaxRateEntry) -> TaxRateView {
    TaxRateView {
        tax_class_id: entry.tax_class_id.to_string(),
        sales_channel: entry.sales_channel.as_wire().to_owned(),
        rate_bps: entry.rate.basis_points(),
        components: entry
            .components
            .iter()
            .map(|component| TaxComponentView {
                name: component.name.clone(),
                rate_bps: component.rate.basis_points(),
            })
            .collect(),
        component_name_invalid: entry
            .components
            .iter()
            .any(|component| TaxComponentName::new(&component.name).is_err()),
    }
}

/// Checks one row's breakdown and returns the components to store
/// ([ADR-0104](../../../../docs/adr/0104-multi-component-and-inclusive-tax.md)).
///
/// The invariant is that the parts sum to the row's own rate, and it is checked **here**, where the
/// table is authored, rather than at the till. A row that fails it would print an invoice whose
/// lines do not add up to the tax charged — which is the one way this feature can produce a document
/// an auditor rejects, and the refusal says which row and by how much so the operator can fix it
/// rather than hunt for it.
///
/// A name, trimmed as it is stored, must be a [`TaxComponentName`], because a settled bill records
/// and the reports sum a component under its name, and record nothing for a name that is not one
/// ([ADR-0168](../../../../docs/adr/0168-a-settled-bill-records-its-tax-components.md) decision 2).
/// The refusal names the row by its class and channel and states the rule, and never the name,
/// which neither a response nor the log carries.
///
/// An empty list always passes: it is "no breakdown", not "a breakdown summing to zero".
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
fn checked_components(row: &TaxRateRowRequest) -> Result<Vec<TaxComponent>, Response> {
    if row.components.is_empty() {
        return Ok(Vec::new());
    }
    if row
        .components
        .iter()
        .any(|component| component.name.trim().is_empty())
    {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "a tax component has no name, and the name is what the invoice prints",
            &[("rates", "MISSING_FIELD")],
        ));
    }
    if row
        .components
        .iter()
        .any(|component| TaxComponentName::new(component.name.trim()).is_err())
    {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!(
                "a tax component of the rate for tax class {} on {} is not named with 2 to 8 \
                 upper-case letters and digits, starting with a letter, such as CGST",
                row.tax_class_id, row.sales_channel
            ),
            &[("rates", "INVALID_FORMAT")],
        ));
    }
    let total: u64 = row
        .components
        .iter()
        .map(|component| u64::from(component.rate_bps))
        .sum();
    if total != u64::from(row.rate_bps) {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!(
                "a rate's components sum to {total} basis points but the rate is {}; the invoice \
                 would print lines that do not add up to the tax charged",
                row.rate_bps
            )
            .as_str(),
            &[("rates", "OUT_OF_RANGE")],
        ));
    }
    Ok(row
        .components
        .iter()
        .map(|component| {
            TaxComponent::new(
                component.name.trim(),
                TaxRate::from_basis_points(component.rate_bps),
            )
        })
        .collect())
}

/// Validates a whole submitted grid into the entries to store, or the one refusal that stops it.
///
/// Every check is on the row rather than on the table: an unknown class or channel, a rate above
/// 100 %, a `(class, channel)` pair submitted twice, and a breakdown that does not sum to its own
/// rate or names a component with anything but a [`TaxComponentName`]. The first failure returns,
/// because a partly-applied grid is not a state this resource has — a save replaces the tenant's
/// whole table or none of it.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
fn checked_entries(
    rows: &[TaxRateRowRequest],
    known: &BTreeSet<TaxClassId>,
) -> Result<Vec<TaxRateEntry>, Response> {
    let mut entries = Vec::with_capacity(rows.len());
    let mut seen: BTreeSet<(TaxClassId, SalesChannel)> = BTreeSet::new();
    for row in rows {
        let tax_class_id = match parse_ulid_fields([("tax_class_id", &row.tax_class_id)]) {
            Ok([tax_class_id]) => TaxClassId::new(tax_class_id),
            Err(refusal) => return Err(refusal),
        };
        if !known.contains(&tax_class_id) {
            return Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "a tax rate names an unknown tax class",
                &[("rates", "UNKNOWN_REFERENCE")],
            ));
        }
        let Some(sales_channel) = SalesChannel::from_wire(&row.sales_channel) else {
            return Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "a tax rate names an unknown sales channel",
                &[("rates", "INVALID_ENUM_VALUE")],
            ));
        };
        if row.rate_bps > MAX_TAX_RATE_BPS {
            return Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "a tax rate exceeds 100%",
                &[("rates", "OUT_OF_RANGE")],
            ));
        }
        if !seen.insert((tax_class_id, sales_channel)) {
            return Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "a (tax class, channel) pair is repeated",
                &[("rates", "DUPLICATE")],
            ));
        }
        entries.push(
            TaxRateEntry::new(
                tax_class_id,
                sales_channel,
                TaxRate::from_basis_points(row.rate_bps),
            )
            .with_components(checked_components(row)?),
        );
    }
    Ok(entries)
}

/// A super-admin lists a tenant's authored tax rates.
async fn admin_list_tax_rates<Tax, Cat, A, C>(
    State(state): State<TaxRateState<Tax, Cat, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Tax: TaxRateStore + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await
    {
        return denied;
    }
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    match state.tax_rates.list_tax_rates(tenant_id).await {
        Ok((rows, version)) => {
            let view: Vec<TaxRateView> = rows.iter().map(tax_rate_view).collect();
            let response = (StatusCode::OK, Json(view)).into_response();
            // The collection is the entity, so its version rides on the response's `ETag` rather
            // than inside the body — the body is a JSON array, which has nowhere to put a field
            // ([ADR-0095](../../../../docs/adr/0095-conditional-writes-for-collections.md)). A tenant
            // that has never saved rates has no version, and sends `If-Match: *` to say so.
            match version {
                Some(version) => with_etag(response, &version),
                None => response,
            }
        }
        Err(error) => tax_rate_error_response(&error),
    }
}

/// A super-admin replaces a tenant's whole tax-rate table.
///
/// Every row is validated before the write: its class must be one the tenant has authored, its channel
/// a token this build knows, its rate no more than 100%, no `(class, channel)` pair repeated, and
/// each component named with a [`TaxComponentName`] — each a `400` naming the fault, so a bad grid
/// never reaches the store as a `500`. A stored row named otherwise is no exception: a save sends
/// the whole grid, so it is refused until that name is changed.
async fn admin_set_tax_rates<Tax, Cat, A, C>(
    State(state): State<TaxRateState<Tax, Cat, A, C>>,
    headers: HeaderMap,
    Json(request): Json<SetTaxRatesRequest>,
) -> Response
where
    Tax: TaxRateStore + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCatalog,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // The version the console read the grid at, or `None` from `If-Match: *` for a tenant that has
    // never saved rates ([ADR-0095](../../../../docs/adr/0095-conditional-writes-for-collections.md)).
    let expected = match if_match_collection(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    let known: BTreeSet<TaxClassId> = match state.catalog.list_tax_classes(tenant_id).await {
        Ok(classes) => classes
            .iter()
            .map(|class| class.record.tax_class_id)
            .collect(),
        Err(error) => return catalog_error_response(&error),
    };
    let entries = match checked_entries(&request.rates, &known) {
        Ok(entries) => entries,
        Err(refusal) => return refusal,
    };
    match state
        .tax_rates
        .set_tax_rates(tenant_id, &entries, expected.as_ref())
        .await
    {
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        // No rate table for this tenant to replace, and the caller named a version for one. There is
        // no other edit to have lost, so a `412` telling them to reload would send them looking for
        // a conflict nobody caused.
        Ok(UpdateOutcome::NotFound) => not_found("tax rate table"),
        Ok(UpdateOutcome::Updated(version)) => {
            let view: Vec<TaxRateView> = entries.iter().map(tax_rate_view).collect();
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "tax_rate.set",
                "tax_rate",
                &tenant_id.to_string(),
                None,
                serde_json::to_value(&view).ok(),
            )
            .await;
            with_etag((StatusCode::OK, Json(view)).into_response(), &version)
        }
        Err(error) => tax_rate_error_response(&error),
    }
}

/// Maps a tax-rate store failure to a retryable `503`, logging the detail rather than leaking it.
pub(super) fn tax_rate_error_response(error: &TaxRateStoreError) -> Response {
    tracing::error!(%error, "a tax-rate store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the tax-rate service is unavailable",
    )
}
