// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/campaigns/{campaign_id}/vouchers`: mint and list a voucher campaign's codes.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error, api_error_with_details, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::campaign::PublishedCampaignKind;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{CampaignId, TenantId, VoucherId};
use serde::Deserialize;

use super::{campaign_entropy_unavailable, campaign_error_response};
use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::campaigns::CampaignStore;
use crate::http::{VoucherListQuery, not_found, paged_ok, parse_page};
use crate::paging::Page;
use crate::vouchers::{NewVoucher, VoucherStore, VoucherStoreError, generate_code};

// --- Vouchers (ADR-0077, Track M3): mint and list the codes a voucher-kind campaign redeems --------

/// The largest batch of vouchers one request may mint. Generous for a real promotion drop, bounded so
/// a typo cannot ask the store to mint millions in one call.
const MAX_VOUCHER_BATCH: u32 = 10_000;

/// The state the voucher routes share: the voucher store, the campaign store (to check the campaign
/// exists and is a voucher-kind), and the admin/clock/audit every `/admin` write needs.
#[derive(Clone)]
struct VoucherState<V, Camp, A, C> {
    vouchers: V,
    campaigns: Camp,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A `POST /admin/campaigns/{id}/vouchers` body: the tenant and how many codes to mint.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
struct GenerateVouchersRequest {
    tenant_id: String,
    count: u32,
}

/// One minted or listed voucher on the wire: the id, the code, and its status.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct VoucherView {
    voucher_id: String,
    code: String,
    status: crate::vouchers::VoucherStatus,
}

/// Builds the voucher sub-router ([ADR-0077](../../../docs/adr/0077-campaigns-and-scheduling.md), M3).
///
/// `POST /admin/campaigns/{id}/vouchers` mints a batch of codes for a voucher-kind campaign and returns
/// them once; `GET` lists a campaign's codes. Both behind [`ConsolePermission::ManageCampaigns`] — a
/// voucher code carries redeemable value, so even listing it is a manage action, not a plain read. The
/// mint audits `voucher.batch.generate` with the count only, never the codes.
pub fn voucher_router<V, Camp, A, C>(
    vouchers: V,
    campaigns: Camp,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    V: VoucherStore + Clone + Send + Sync + 'static,
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/campaigns/{campaign_id}/vouchers",
            get(admin_list_vouchers::<V, Camp, A, C>)
                .post(admin_generate_vouchers::<V, Camp, A, C>),
        )
        .with_state(VoucherState {
            vouchers,
            campaigns,
            admin,
            clock,
            audit,
        })
}

#[utoipa::path(
    post,
    path = "/admin/campaigns/{campaign_id}/vouchers",
    extensions(("x-permission" = json!("console.campaigns.manage"))),
    security(("session_cookie" = [])),
    params(
        ("campaign_id" = String, Path, description = "The campaign's 26-character ULID"),
    ),
    request_body(
        content = GenerateVouchersRequest, description = "`tenant_id` and `count`, how many codes to mint, from 1 to 10000",
        content_type = "application/json",
    ),
    responses(
        (status = 201, body = [VoucherView], description = "Minted, as an array: each code's `voucher_id`, `code` and \
                                      `status`, `active`. A code carries redeemable value; the audit \
                                      entry records the count, never the codes"),
        (status = 400, description = "An id is not a ULID, count is out of range, or the campaign \
                                      is not a voucher campaign", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.campaigns.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no campaign with that id", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The campaign store, the voucher store or the entropy source \
                                      is unavailable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "campaigns",
)]
/// A super-admin mints a batch of voucher codes for a voucher-kind campaign.
async fn admin_generate_vouchers<V, Camp, A, C>(
    State(state): State<VoucherState<V, Camp, A, C>>,
    headers: HeaderMap,
    Path(campaign_id): Path<String>,
    Json(request): Json<GenerateVouchersRequest>,
) -> Response
where
    V: VoucherStore + Clone + Send + Sync + 'static,
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCampaigns,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, campaign_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("campaign_id", &campaign_id),
    ]) {
        Ok([tenant_id, campaign_id]) => (TenantId::new(tenant_id), CampaignId::new(campaign_id)),
        Err(refusal) => return refusal,
    };
    if request.count == 0 || request.count > MAX_VOUCHER_BATCH {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "count must be between 1 and 10000",
            &[("count", "OUT_OF_RANGE")],
        );
    }
    // The campaign must exist and be a voucher-kind — a code only makes sense for one the engine
    // evaluates as a voucher.
    match state.campaigns.get_campaign(tenant_id, campaign_id).await {
        Ok(Some(campaign)) if campaign.record.kind == PublishedCampaignKind::Voucher => {}
        Ok(Some(_other)) => {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                "campaign is not a voucher-kind campaign",
                &[("campaign_id", "WRONG_KIND")],
            );
        }
        Ok(None) => return not_found("campaign"),
        Err(error) => return campaign_error_response(&error),
    }
    // Mint the batch — a fresh id and code each, failing closed if OS entropy is unavailable.
    let mut minted = Vec::with_capacity(request.count as usize);
    for _ in 0..request.count {
        let (Some(voucher_id), Some(code)) = (
            mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(VoucherId::new),
            generate_code(),
        ) else {
            return campaign_entropy_unavailable();
        };
        minted.push(NewVoucher {
            voucher_id,
            campaign_id,
            code,
        });
    }
    match state.vouchers.insert_batch(tenant_id, &minted).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "voucher.batch.generate",
                "campaign",
                &campaign_id.to_string(),
                None,
                // The count only — never the codes, which carry redeemable value.
                serde_json::to_value(minted.len()).ok(),
            )
            .await;
            let view: Vec<VoucherView> = minted
                .iter()
                .map(|voucher| VoucherView {
                    voucher_id: voucher.voucher_id.to_string(),
                    code: voucher.code.clone(),
                    status: crate::vouchers::VoucherStatus::Active,
                })
                .collect();
            (StatusCode::CREATED, Json(view)).into_response()
        }
        Err(error) => voucher_error_response(&error),
    }
}

#[utoipa::path(
    get,
    path = "/admin/campaigns/{campaign_id}/vouchers",
    extensions(("x-permission" = json!("console.campaigns.manage"))),
    security(("session_cookie" = [])),
    params(
        ("campaign_id" = String, Path, description = "The campaign's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant that owns it (a 26-character ULID)"),
        ("limit" = Option<u32>, Query, description = "How many codes to return, 1 to 500. Absent, \
                                                    every code as an array rather than a page"),
        ("offset" = Option<u32>, Query, description = "How many codes to skip, 0 to 100000. Only \
                                                     with `limit`"),
    ),
    responses(
        (status = 200, body = [VoucherView], description = "Without `limit`, every code as an array; with it, a page: \
                                      `items`, `total`, `limit` and `offset`. Each code has \
                                      `voucher_id`, `code` and `status` (`active`, `redeemed` or \
                                      `void`). Listing codes needs the manage permission, because a \
                                      code carries redeemable value"),
        (status = 400, description = "tenant_id or campaign_id is absent or not a ULID, limit or \
                                      offset is not an integer in range, or offset is sent without \
                                      limit", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.campaigns.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The voucher store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "campaigns",
)]
/// A super-admin lists a campaign's minted voucher codes.
async fn admin_list_vouchers<V, Camp, A, C>(
    State(state): State<VoucherState<V, Camp, A, C>>,
    headers: HeaderMap,
    Path(campaign_id): Path<String>,
    Query(query): Query<VoucherListQuery>,
) -> Response
where
    V: VoucherStore + Clone + Send + Sync + 'static,
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageCampaigns,
    )
    .await
    {
        return denied;
    }
    let (tenant_id, campaign_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("campaign_id", &campaign_id),
    ]) {
        Ok([tenant_id, campaign_id]) => (TenantId::new(tenant_id), CampaignId::new(campaign_id)),
        Err(refusal) => return refusal,
    };
    // Two reads, chosen by whether the caller named a limit. Not a default and not a migration: an
    // operator printing a promotion's flyer run wants every code, and the console's table wants
    // twenty-five of them (ADR-0098).
    let Some(page) = parse_page(query.limit.as_deref(), query.offset.as_deref()) else {
        return match state
            .vouchers
            .list_by_campaign(tenant_id, campaign_id)
            .await
        {
            Ok(records) => (StatusCode::OK, Json(voucher_views(records))).into_response(),
            Err(error) => voucher_error_response(&error),
        };
    };
    let page = match page {
        Ok(page) => page,
        Err(refusal) => return refusal,
    };
    match state
        .vouchers
        .list_by_campaign_page(tenant_id, campaign_id, page)
        .await
    {
        Ok(read) => paged_ok(Page::new(voucher_views(read.items), read.total), page),
        Err(error) => voucher_error_response(&error),
    }
}

/// The wire view of a batch of vouchers.
///
/// Shared by the paged and unpaged reads, so a row cannot render one way on the table and another on
/// the flyer run — and so neither form can start leaking a field the other withholds.
fn voucher_views(records: Vec<crate::vouchers::VoucherRecord>) -> Vec<VoucherView> {
    records
        .into_iter()
        .map(|record| VoucherView {
            voucher_id: record.voucher_id,
            code: record.code,
            status: record.status,
        })
        .collect()
}

/// Maps a voucher store failure to a retryable `503`, logging the detail rather than leaking it.
fn voucher_error_response(error: &VoucherStoreError) -> Response {
    tracing::error!(%error, "a voucher store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the voucher service is unavailable",
    )
}
