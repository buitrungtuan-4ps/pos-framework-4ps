// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /admin/einvoices` and `POST /admin/einvoices/{invoice_id}/correct`: the console's invoice list
//! and its corrections
//! ([ADR-0241](../../../../docs/adr/0241-the-cloud-submits-each-issued-invoice-from-the-log-and-keeps-its-state.md)
//! decision 5, [ADR-0225](../../../../docs/adr/0225-a-vietnamese-store-issues-its-e-invoice-and-the-cloud-submits-it.md)
//! decision 7).
//!
//! A paid bill with an issued invoice is corrected here, never voided at the store. A correction is a
//! new invoice, an adjustment or a replacement, numbered under the tenant's correction symbol (which
//! only the cloud issues from, ADR-0240 decision 6), recorded as `PENDING` and submitted like any
//! other. Which errors need which kind is to be confirmed by legal or the accountant.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{
    api_error, api_error_with_details, parse_ulid_fields, service_unavailable,
};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;
use serde::{Deserialize, Serialize};
use store_postgres::SubmissionRow;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::einvoice::feed::{STATE_PENDING, SubmissionStore};
use crate::einvoice::symbols::{SymbolLedger, allocate};
use crate::http::Json;
use crate::http::config_nodes::utc_year;

#[derive(Clone)]
pub(crate) struct EInvoicesState<Cfg, A, C> {
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    submissions: Arc<dyn SubmissionStore>,
    ledger: Arc<dyn SymbolLedger>,
}

/// Builds the invoice list and correction sub-router.
pub fn einvoices_router<Cfg, A, C>(
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    submissions: Arc<dyn SubmissionStore>,
    ledger: Arc<dyn SymbolLedger>,
) -> Router
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/einvoices", get(admin_list_einvoices::<Cfg, A, C>))
        .route(
            "/admin/einvoices/{invoice_id}/correct",
            post(admin_correct_einvoice::<Cfg, A, C>),
        )
        .with_state(EInvoicesState {
            config_trees,
            admin,
            clock,
            audit,
            submissions,
            ledger,
        })
}

/// `GET /admin/einvoices` query.
#[derive(Debug, Clone, Deserialize, utoipa::IntoParams)]
pub(crate) struct ListQuery {
    tenant_id: String,
    store_id: Option<String>,
}

/// One invoice and where its submission stands. No buyer is named.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct EInvoiceView {
    invoice_id: String,
    store_id: String,
    bill_id: String,
    /// `1C26MAA/00000042`.
    invoice_number: String,
    /// `ORIGINAL`, `ADJUSTMENT` or `REPLACEMENT`.
    kind: String,
    /// `YYYY-MM-DD`.
    issued_on: String,
    total_minor: i64,
    currency: String,
    /// `PENDING`, `RETRYING`, `SUBMITTED` or `REJECTED`.
    state: String,
    failed_attempts: i32,
    next_attempt_ms: i64,
    #[schema(required = true)]
    authority_reference: Option<String>,
    #[schema(required = true)]
    refusal: Option<String>,
    #[schema(required = true)]
    corrects_invoice_number: Option<String>,
    recorded_at_ms: i64,
    updated_at_ms: i64,
}

impl From<SubmissionRow> for EInvoiceView {
    fn from(row: SubmissionRow) -> Self {
        Self {
            invoice_id: row.invoice_id,
            store_id: row.store_id,
            bill_id: row.bill_id,
            invoice_number: row.invoice_number,
            kind: row.kind,
            issued_on: row.issued_on,
            total_minor: row.total_minor,
            currency: row.currency,
            state: row.state,
            failed_attempts: row.failed_attempts,
            next_attempt_ms: row.next_attempt_ms,
            authority_reference: row.authority_reference,
            refusal: row.refusal,
            corrects_invoice_number: row.corrects_invoice_number,
            recorded_at_ms: row.recorded_at_ms,
            updated_at_ms: row.updated_at_ms,
        }
    }
}

#[utoipa::path(
    get,
    path = "/admin/einvoices",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    params(ListQuery),
    responses(
        (status = 200, body = [EInvoiceView], description = "The tenant's invoices, or one store's, newest first, at most 1,000, \
                                      each with where its submission stands. No buyer is named"),
        (status = 400, description = "tenant_id or store_id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The submissions are unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "e-invoices",
)]
/// The console's invoice list (ADR-0241 decision 5).
pub(crate) async fn admin_list_einvoices<Cfg, A, C>(
    State(state): State<EInvoicesState<Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Response, Response>
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::Read,
    )
    .await?;
    let [tenant] = parse_ulid_fields([("tenant_id", &query.tenant_id)])?;
    let store = match query.store_id.as_deref().filter(|id| !id.is_empty()) {
        Some(id) => Some(StoreId::new(parse_ulid_fields([("store_id", id)])?[0])),
        None => None,
    };
    let rows = state
        .submissions
        .list(TenantId::new(tenant), store, false)
        .await
        .map_err(|_| service_unavailable("e-invoice submissions"))?;
    let views: Vec<EInvoiceView> = rows.into_iter().map(EInvoiceView::from).collect();
    Ok((StatusCode::OK, Json(views)).into_response())
}

/// A `POST /admin/einvoices/{invoice_id}/correct` body.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(crate) struct CorrectEInvoiceRequest {
    tenant_id: String,
    /// `ADJUSTMENT` (the amount up or down, a whole refund included) or `REPLACEMENT`.
    kind: String,
    /// For an adjustment, the change in minor units, negative for down; for a replacement, the
    /// replacement's total.
    total_minor: i64,
}

#[utoipa::path(
    post,
    path = "/admin/einvoices/{invoice_id}/correct",
    extensions(("x-permission" = json!("console.einvoices.correct"))),
    security(("session_cookie" = [])),
    params(("invoice_id" = String, Path, description = "The invoice to correct (a ULID)")),
    request_body(content = CorrectEInvoiceRequest, content_type = "application/json",
        description = "The tenant, the kind (`ADJUSTMENT` or `REPLACEMENT`) and the total"),
    responses(
        (status = 201, body = EInvoiceView, description = "The correction, numbered under the tenant's correction symbol, \
                                      recorded as PENDING and audited. It is submitted by the next pass"),
        (status = 400, description = "An id is not a ULID, or the kind is not one of the two (`INVALID_FORMAT` on `kind`)", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.einvoices.correct", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "No such invoice in the tenant", body = crate::openapi_admin::ErrorResponse),
        (status = 422, description = "The store's country has no symbol rule for the correction symbol \
                                      (`E_INVOICE_SYMBOL_NO_RULE` on `symbol`)", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The submissions or the ledger are unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "e-invoices",
)]
/// Records a correction of an issued invoice (ADR-0225 decision 7, ADR-0241 decision 5).
pub(crate) async fn admin_correct_einvoice<Cfg, A, C>(
    State(state): State<EInvoicesState<Cfg, A, C>>,
    headers: HeaderMap,
    Path(invoice_id): Path<String>,
    Json(request): Json<CorrectEInvoiceRequest>,
) -> Result<Response, Response>
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::CorrectInvoices,
    )
    .await?;
    let [tenant, invoice] = parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("invoice_id", &invoice_id),
    ])?;
    let tenant = TenantId::new(tenant);
    if !matches!(request.kind.as_str(), "ADJUSTMENT" | "REPLACEMENT") {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "kind is ADJUSTMENT or REPLACEMENT",
            &[("kind", "INVALID_FORMAT")],
        ));
    }
    let original = state
        .submissions
        .get(tenant, invoice)
        .await
        .map_err(|_| service_unavailable("e-invoice submissions"))?
        .ok_or_else(|| api_error(ErrorStatus::NotFound, "no such invoice"))?;
    let now = state.clock.now();
    let symbol = correction_symbol(&state, tenant, &original, now).await?;
    let last = state
        .submissions
        .last_number(tenant, symbol.clone())
        .await
        .map_err(|_| service_unavailable("e-invoice submissions"))?;
    let sequence = last
        .and_then(|number| {
            number
                .rsplit('/')
                .next()
                .and_then(|n| n.parse::<u32>().ok())
        })
        .unwrap_or(0)
        .saturating_add(1);
    let now_ms = now.as_milliseconds_since_epoch();
    let id = mint_ulid(now_ms).ok_or_else(|| service_unavailable("id minting"))?;
    let correction = SubmissionRow {
        invoice_id: id.to_string(),
        invoice_number: format!("{symbol}/{sequence:08}"),
        kind: request.kind.clone(),
        issued_on: utc_date(now_ms),
        total_minor: request.total_minor,
        buyer_subject_id: None,
        corrects_invoice_number: Some(original.invoice_number.clone()),
        state: STATE_PENDING.to_owned(),
        failed_attempts: 0,
        next_attempt_ms: now_ms,
        authority_reference: None,
        refusal: None,
        recorded_at_ms: now_ms,
        updated_at_ms: now_ms,
        ..original.clone()
    };
    state
        .submissions
        .record(correction.clone())
        .await
        .map_err(|_| service_unavailable("e-invoice submissions"))?;
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant),
        "einvoice.correct",
        "einvoice",
        &original.invoice_id,
        None,
        Some(serde_json::json!({
            "kind": correction.kind,
            "invoice_number": correction.invoice_number,
            "corrects_invoice_number": original.invoice_number,
            "total_minor": correction.total_minor,
        })),
    )
    .await;
    Ok((StatusCode::CREATED, Json(EInvoiceView::from(correction))).into_response())
}

/// The tenant's correction symbol for this year: the ledger's, or a new one allocated from the
/// original store's country rule (ADR-0240 decision 6).
async fn correction_symbol<Cfg, A, C>(
    state: &EInvoicesState<Cfg, A, C>,
    tenant: TenantId,
    original: &SubmissionRow,
    now: pos_proto::time::Timestamp,
) -> Result<String, Response>
where
    Cfg: ConfigTreeStore,
{
    let year = utc_year(now);
    let year_digits = format!("{:02}", year.rem_euclid(100));
    let held = state
        .ledger
        .list(tenant)
        .await
        .map_err(|_| service_unavailable("e-invoice symbol ledger"))?;
    if let Some(record) = held.iter().find(|record| {
        record.store_id.is_none() && record.symbol.get(2..4) == Some(year_digits.as_str())
    }) {
        return Ok(record.symbol.clone());
    }
    let store = original
        .store_id
        .parse::<Ulid>()
        .map(StoreId::new)
        .map_err(|_| service_unavailable("e-invoice submissions"))?;
    let tree = state
        .config_trees
        .load(tenant, store)
        .await
        .map_err(|_| service_unavailable("configuration"))?;
    let countries = crate::countries::registry();
    let country = tree
        .as_ref()
        .and_then(|tree| crate::http::locale_node::published_country(&tree.record))
        .and_then(|code| countries.get(code));
    allocate(
        state.ledger.as_ref(),
        country,
        tenant,
        None,
        None,
        year,
        now,
    )
    .await
    .map(|record| record.symbol)
    .map_err(|_| {
        api_error_with_details(
            ErrorStatus::Unprocessable,
            "no correction symbol could be allocated from the store's country rule",
            &[("symbol", "E_INVOICE_SYMBOL_NO_RULE")],
        )
    })
}

/// `YYYY-MM-DD` of `ms` in UTC.
fn utc_date(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_from_march = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_from_march + 2) / 5 + 1;
    let month = if month_from_march < 10 {
        month_from_march + 3
    } else {
        month_from_march - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::utc_date;

    #[test]
    fn a_utc_date_is_written_as_the_calendar_writes_it() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(1_795_564_800_000), "2026-11-25");
        assert_eq!(utc_date(1_798_761_599_999), "2026-12-31");
        assert_eq!(utc_date(1_772_323_200_000), "2026-03-01");
    }
}
