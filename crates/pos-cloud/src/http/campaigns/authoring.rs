// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `/admin/campaigns` handlers: list, read, create, update and delete.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{CampaignId, TenantId};

use super::{
    CampaignAuditSummary, CampaignRequest, CampaignState, build_campaign,
    campaign_entropy_unavailable, campaign_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::campaigns::CampaignStore;
use crate::http::{
    RegistryTenantQuery, already_exists, if_match, not_found, version_mismatch, versioned_created,
    versioned_ok,
};
use crate::version::{CreateOutcome, UpdateOutcome};

/// A super-admin lists a tenant's authored campaigns.
pub(super) async fn admin_list_campaigns<Camp, A, C>(
    State(state): State<CampaignState<Camp, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Camp: CampaignStore + Clone + Send + Sync + 'static,
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
    match state.campaigns.list_campaigns(tenant_id).await {
        Ok(campaigns) => (StatusCode::OK, Json(campaigns)).into_response(),
        Err(error) => campaign_error_response(&error),
    }
}

/// A super-admin reads one campaign by id.
pub(super) async fn admin_get_campaign<Camp, A, C>(
    State(state): State<CampaignState<Camp, A, C>>,
    headers: HeaderMap,
    Path(campaign_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    Camp: CampaignStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, campaign_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("campaign_id", &campaign_id),
    ]) {
        Ok([tenant_id, campaign_id]) => (TenantId::new(tenant_id), CampaignId::new(campaign_id)),
        Err(refusal) => return refusal,
    };
    match state.campaigns.get_campaign(tenant_id, campaign_id).await {
        Ok(Some(campaign)) => (StatusCode::OK, Json(campaign)).into_response(),
        Ok(None) => not_found("campaign"),
        Err(error) => campaign_error_response(&error),
    }
}

/// A super-admin creates a campaign; the server mints its id.
pub(super) async fn admin_create_campaign<Camp, A, C>(
    State(state): State<CampaignState<Camp, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CampaignRequest>,
) -> Response
where
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let Some(campaign_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(CampaignId::new)
    else {
        return campaign_entropy_unavailable();
    };
    let campaign = match build_campaign(&request, campaign_id) {
        Ok(campaign) => campaign,
        Err(refusal) => return refusal.into_response(),
    };
    match state.campaigns.create_campaign(tenant_id, &campaign).await {
        Ok(CreateOutcome::Created(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "campaign.create",
                "campaign",
                &campaign_id.to_string(),
                None,
                serde_json::to_value(CampaignAuditSummary::of(&campaign)).ok(),
            )
            .await;
            versioned_created(campaign, &version)
        }
        // Unreachable while the id is minted here rather than sent by the caller, and answered
        // anyway: the seam cannot know that, and a route that mints its own id today may take one
        // tomorrow. Silently overwriting is what this slice removed.
        Ok(CreateOutcome::AlreadyExists) => already_exists("campaign"),
        Err(error) => campaign_error_response(&error),
    }
}

/// A super-admin updates a campaign in place, by the path id.
pub(super) async fn admin_update_campaign<Camp, A, C>(
    State(state): State<CampaignState<Camp, A, C>>,
    headers: HeaderMap,
    Path(campaign_id): Path<String>,
    Json(request): Json<CampaignRequest>,
) -> Response
where
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
    let before = match state.campaigns.get_campaign(tenant_id, campaign_id).await {
        Ok(Some(existing)) => existing,
        Ok(None) => return not_found("campaign"),
        Err(error) => return campaign_error_response(&error),
    };
    let campaign = match build_campaign(&request, campaign_id) {
        Ok(campaign) => campaign,
        Err(refusal) => return refusal.into_response(),
    };
    let expected = match if_match(&headers) {
        Ok(expected) => expected,
        Err(refusal) => return refusal,
    };
    match state
        .campaigns
        .update_campaign(tenant_id, &campaign, &expected)
        .await
    {
        Ok(UpdateOutcome::Updated(version)) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "campaign.update",
                "campaign",
                &campaign_id.to_string(),
                serde_json::to_value(CampaignAuditSummary::of(&before.record)).ok(),
                serde_json::to_value(CampaignAuditSummary::of(&campaign)).ok(),
            )
            .await;
            versioned_ok(campaign, &version)
        }
        Ok(UpdateOutcome::VersionMismatch) => version_mismatch(),
        Ok(UpdateOutcome::NotFound) => not_found("campaign"),
        Err(error) => campaign_error_response(&error),
    }
}

/// A super-admin deletes a campaign by id.
pub(super) async fn admin_delete_campaign<Camp, A, C>(
    State(state): State<CampaignState<Camp, A, C>>,
    headers: HeaderMap,
    Path(campaign_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
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
        ("tenant_id", &query.tenant_id),
        ("campaign_id", &campaign_id),
    ]) {
        Ok([tenant_id, campaign_id]) => (TenantId::new(tenant_id), CampaignId::new(campaign_id)),
        Err(refusal) => return refusal,
    };
    let before = match state.campaigns.get_campaign(tenant_id, campaign_id).await {
        Ok(Some(existing)) => existing,
        Ok(None) => return not_found("campaign"),
        Err(error) => return campaign_error_response(&error),
    };
    match state
        .campaigns
        .delete_campaign(tenant_id, campaign_id)
        .await
    {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "campaign.delete",
                "campaign",
                &campaign_id.to_string(),
                serde_json::to_value(CampaignAuditSummary::of(&before.record)).ok(),
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(error) => campaign_error_response(&error),
    }
}
