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
use pos_proto::einvoice::EInvoiceKind;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;
use serde::{Deserialize, Serialize};
use store_postgres::SubmissionRow;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::einvoice::corrections::{Correction, CorrectionError, record_correction};
use crate::einvoice::feed::SubmissionStore;
use crate::einvoice::requests::CorrectionRequests;
use crate::einvoice::symbols::SymbolLedger;
use crate::http::Json;

#[derive(Clone)]
pub(crate) struct EInvoicesState<Cfg, A, C> {
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    submissions: Arc<dyn SubmissionStore>,
    ledger: Arc<dyn SymbolLedger>,
    requests: Arc<dyn CorrectionRequests>,
}

pub(crate) mod correction_requests;

/// Builds the invoice list and correction sub-router, with the queue of the stores' correction
/// requests (ADR-0250).
pub fn einvoices_router<Cfg, A, C>(
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    submissions: Arc<dyn SubmissionStore>,
    ledger: Arc<dyn SymbolLedger>,
    requests: Arc<dyn CorrectionRequests>,
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
        .route(
            "/admin/einvoices/correction-requests",
            get(correction_requests::admin_list_correction_requests::<Cfg, A, C>),
        )
        .route(
            "/admin/einvoices/correction-requests/{request_id}/approve",
            post(correction_requests::admin_approve_correction_request::<Cfg, A, C>),
        )
        .route(
            "/admin/einvoices/correction-requests/{request_id}/decline",
            post(correction_requests::admin_decline_correction_request::<Cfg, A, C>),
        )
        .with_state(EInvoicesState {
            config_trees,
            admin,
            clock,
            audit,
            submissions,
            ledger,
            requests,
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
        (status = 400, description = "An id is not a ULID, the kind is not one of the two (`INVALID_FORMAT` on `kind`), \
                                      or an adjustment of zero, a negative replacement, or an adjustment that takes the invoice below zero (`OUT_OF_RANGE` on `total_minor`)", body = crate::openapi_admin::ErrorResponse),
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
    check_correction(&request)?;
    let original = state
        .submissions
        .get(tenant, invoice)
        .await
        .map_err(|_| service_unavailable("e-invoice submissions"))?
        .ok_or_else(|| api_error(ErrorStatus::NotFound, "no such invoice"))?;
    if crate::einvoice::vendor_numbers::is_pending(&original.invoice_number) {
        // A correction names the number it corrects, which the vendor has not yet given (ADR-0251).
        return Err(api_error_with_details(
            ErrorStatus::FailedPrecondition,
            "the invoice waits for the vendor's number",
            &[("invoice_id", "E_INVOICE_NUMBER_PENDING")],
        ));
    }
    let kind = if request.kind == "REPLACEMENT" {
        EInvoiceKind::Replacement
    } else {
        EInvoiceKind::Adjustment
    };
    let now = state.clock.now();
    let id = mint_ulid(now.as_milliseconds_since_epoch())
        .ok_or_else(|| service_unavailable("id minting"))?;
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
    let correction = record_correction(
        state.submissions.as_ref(),
        state.ledger.as_ref(),
        country,
        &original,
        Correction {
            kind,
            total_minor: request.total_minor,
        },
        id,
        now,
    )
    .await
    .map_err(refusal)?;
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

/// Refuses a correction that is not one: an unknown kind, an adjustment of nothing, or a negative
/// replacement (security review E1).
#[expect(
    clippy::result_large_err,
    reason = "the Err is the axum Response the handler returns"
)]
fn check_correction(request: &CorrectEInvoiceRequest) -> Result<(), Response> {
    if !matches!(request.kind.as_str(), "ADJUSTMENT" | "REPLACEMENT") {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "kind is ADJUSTMENT or REPLACEMENT",
            &[("kind", "INVALID_FORMAT")],
        ));
    }
    // An adjustment of nothing and a replacement below zero are not corrections (security review
    // E1): the first would take a number for no change, the second would invoice a negative sale.
    let empty = request.kind == "ADJUSTMENT" && request.total_minor == 0;
    if empty || (request.kind == "REPLACEMENT" && request.total_minor < 0) {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "an adjustment changes the amount, and a replacement's total is not negative",
            &[("total_minor", "OUT_OF_RANGE")],
        ));
    }
    Ok(())
}

/// The answer to a correction the shared path refused.
fn refusal(error: CorrectionError) -> Response {
    match error {
        // Found in the phase-10 demo, after security review E1: a correction may take a sale back
        // to nothing, never further. Whether an adjustment may exceed what was sold is to be
        // confirmed by legal or the accountant; this keeps to the conservative reading.
        CorrectionError::BelowZero => api_error_with_details(
            ErrorStatus::InvalidArgument,
            "an adjustment cannot take the invoice below zero",
            &[("total_minor", "OUT_OF_RANGE")],
        ),
        CorrectionError::NoRule => api_error_with_details(
            ErrorStatus::Unprocessable,
            "no correction symbol could be allocated from the store's country rule",
            &[("symbol", "E_INVOICE_SYMBOL_NO_RULE")],
        ),
        CorrectionError::Unavailable(what) => service_unavailable(what),
    }
}
