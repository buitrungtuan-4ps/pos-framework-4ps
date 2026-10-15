// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Campaigns: the promotions a tenant authors under `/admin/campaigns`, their publish to a store,
//! the voucher codes a voucher campaign redeems, and effective-dated publishes
//! ([ADR-0077](../../../../docs/adr/0077-campaigns-and-scheduling.md)).
//!
//! The state, the request body and its checks, the router and the store-error helpers are here.

use std::sync::Arc;

use authoring::{
    admin_create_campaign, admin_delete_campaign, admin_get_campaign, admin_list_campaigns,
    admin_update_campaign,
};
use axum::Router;
use axum::response::Response;
use axum::routing::get;
use pos_cloud_api::response::api_error;
use pos_proto::ErrorStatus;
use pos_proto::campaign::{
    PublishedAction, PublishedCampaign, PublishedCampaignKind, PublishedConditions,
};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::CampaignId;
use pos_proto::text::DisplayName;
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::campaigns::{CampaignStore, CampaignStoreError};
use crate::http::FieldRefusal;

mod authoring;
mod publish;
mod scheduled;
mod vouchers;

pub(super) use publish::campaigns_nodes;
pub use publish::config_campaigns_router;
pub(super) use scheduled::scheduled_error_response;
pub use scheduled::scheduled_publish_router;
pub use vouchers::voucher_router;

// --- Campaigns (ADR-0077, Track M3): author the promotions the edge's pricing engine evaluates ----

/// The largest minute-of-day a schedule window accepts (exclusive) — a day has 1440 minutes.
const MINUTES_PER_DAY: u16 = 24 * 60;

/// The state the campaign routes share: the campaign store they read and write, and the
/// admin/clock/audit every `/admin` write needs.
#[derive(Clone)]
struct CampaignState<Camp, A, C> {
    campaigns: Camp,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A create/update body: the tenant and the campaign's authoring fields. The id is **server-owned** —
/// minted on create, the path id on update — so a client never supplies or forges it. Everything else
/// is the wire campaign's own shape (kind, action, conditions), reused rather than re-declared.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct CampaignRequest {
    tenant_id: String,
    name: String,
    kind: PublishedCampaignKind,
    priority: i32,
    #[serde(default)]
    exclusion_group: Option<u16>,
    action: PublishedAction,
    #[serde(default)]
    conditions: PublishedConditions,
    #[serde(default)]
    quota_remaining: Option<u32>,
}

/// A compact audit summary of a campaign: enough to see *what* changed, without reproducing the exact
/// discount terms (T2 configuration) in the queryable audit trail — the terms live in the campaign
/// store, the trail records that they moved.
#[derive(Debug, Clone, serde::Serialize)]
struct CampaignAuditSummary {
    campaign_id: String,
    name: String,
    kind: PublishedCampaignKind,
    priority: i32,
}

impl CampaignAuditSummary {
    fn of(campaign: &PublishedCampaign) -> Self {
        Self {
            campaign_id: campaign.id.to_string(),
            name: campaign.name.as_str().to_owned(),
            kind: campaign.kind,
            priority: campaign.priority,
        }
    }
}

/// Builds the campaigns sub-router ([ADR-0077](../../../../docs/adr/0077-campaigns-and-scheduling.md), M3).
///
/// Per-campaign CRUD over a tenant's promotions. `GET` (list and by-id) is behind
/// [`ConsolePermission::Read`]; `POST`/`PUT`/`DELETE` are behind [`ConsolePermission::ManageCampaigns`]
/// and audited (`campaign.create`/`update`/`delete`) with a summary, never the discount terms. The id
/// is server-owned. Publishing these campaigns to a store is a separate route that reuses
/// `PublishConfig` (a later slice).
pub fn campaign_router<Camp, A, C>(
    campaigns: Camp,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Camp: CampaignStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/campaigns",
            get(admin_list_campaigns::<Camp, A, C>).post(admin_create_campaign::<Camp, A, C>),
        )
        .route(
            "/admin/campaigns/{campaign_id}",
            get(admin_get_campaign::<Camp, A, C>)
                .put(admin_update_campaign::<Camp, A, C>)
                .delete(admin_delete_campaign::<Camp, A, C>),
        )
        .with_state(CampaignState {
            campaigns,
            admin,
            clock,
            audit,
        })
}

/// Validates a campaign request and builds the campaign with the given (server-owned) id. Returns the
/// fault message for a `400` rather than the whole response, so the error type stays small (a
/// `Result<_, Response>` trips `clippy::result_large_err`).
pub(super) fn build_campaign(
    request: &CampaignRequest,
    id: CampaignId,
) -> Result<PublishedCampaign, FieldRefusal> {
    let name = request.name.trim();
    if name.is_empty() {
        return Err(FieldRefusal::new("name is required", "name", "REQUIRED"));
    }
    // Split, where this used to be one `if` ORing both bounds and naming neither. The old message
    // was the ULID-slice pathology in its worse form: not "names every field it looked at" but
    // "names none of them", so a caller who sent both minutes could not tell which was refused.
    if let Some(schedule) = &request.conditions.schedule {
        if schedule.start_minute >= MINUTES_PER_DAY {
            return Err(FieldRefusal::new(
                "a schedule start minute must be within the day (0..1440)",
                "conditions.schedule.start_minute",
                "OUT_OF_RANGE",
            ));
        }
        if schedule.end_minute >= MINUTES_PER_DAY {
            return Err(FieldRefusal::new(
                "a schedule end minute must be within the day (0..1440)",
                "conditions.schedule.end_minute",
                "OUT_OF_RANGE",
            ));
        }
    }
    if let Some(channels) = &request.conditions.channels
        && channels
            .iter()
            .any(|channel| channel.is_unrecognised() || channel.is_unspecified())
    {
        // The collection, not an index: no shipped detail in this file names an element position,
        // and the tax-rate loop sets the precedent for naming the list.
        return Err(FieldRefusal::new(
            "a campaign names an unknown sales channel",
            "conditions.channels",
            "INVALID_ENUM_VALUE",
        ));
    }
    match request.action {
        PublishedAction::Percentage { rate } if rate.numerator() < 0 => {
            return Err(FieldRefusal::new(
                "a percentage rate cannot be negative",
                "action.rate",
                "OUT_OF_RANGE",
            ));
        }
        PublishedAction::AmountOff { amount } if amount.is_negative() => {
            return Err(FieldRefusal::new(
                "an amount off cannot be negative",
                "action.amount",
                "OUT_OF_RANGE",
            ));
        }
        _ => {}
    }
    Ok(PublishedCampaign {
        id,
        name: DisplayName::new(name),
        kind: request.kind,
        priority: request.priority,
        exclusion_group: request.exclusion_group,
        action: request.action,
        conditions: request.conditions.clone(),
        quota_remaining: request.quota_remaining,
    })
}

/// `503` when OS entropy is unavailable to mint a campaign id — the request cannot proceed and a retry
/// may succeed.
fn campaign_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy to mint a campaign id");
    api_error(
        ErrorStatus::Unavailable,
        "the campaign service is unavailable",
    )
}

/// Maps a campaign store failure to a retryable `503`, logging the detail rather than leaking it.
fn campaign_error_response(error: &CampaignStoreError) -> Response {
    tracing::error!(%error, "a campaign store operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the campaign service is unavailable",
    )
}
