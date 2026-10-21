// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/stores/{store_id}/region-acknowledgement`: a store's region difference, and its
//! acknowledgement.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error, api_error_with_details, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::locale::CountryCode;
use pos_proto::time::Timestamp;
use serde::Deserialize;

use super::{FleetState, RegionAcknowledgementView, fleet_error_response};
use crate::auth::admin::{AdminContext, AdminStore};
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigTreeState, ConfigTreeStore};
use crate::fleet::FleetStore;
use crate::http::{Json, not_found};
use crate::lease::{RegionAcknowledgement, RegionAcknowledgementStore};

/// What a console sends to answer a store's region warning
/// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// **The two country codes are the precondition, and they are in the body rather than in an
/// `If-Match` header.** ADR-0114's Consequences said `If-Match`; the code does not, and the reason
/// is the one [`LeaseStore::settle_handover`] already gives one table over: *the named value is the
/// precondition*, and here it is strictly stronger than any version could be. There is no single
/// resource whose `ETag` covers both halves of this comparison — the region lives on `store_lease`
/// and the country lives in the config tree, two rows with two independent writers — so an
/// `If-Match` would have to name one of them and silently miss changes to the other. Naming both
/// codes catches every move of either, and says in the request exactly what the admin was looking at
/// when they decided. The departure is written up in the record's delivery note.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct AcknowledgeRegionRequest {
    /// The tenant the store belongs to.
    tenant_id: String,
    /// The country the console showed as the store's own when the admin decided.
    profile_country: String,
    /// The country the console showed the store's data resting in when the admin decided.
    region_country: String,
    /// Why the difference is correct. Required, non-blank, at most [`REGION_REASON_LIMIT`]
    /// characters. `#[serde(default)]` so an omitted field is refused through the AIP-193 envelope
    /// naming it, rather than by axum's extractor as a bare `422`.
    #[serde(default)]
    reason: String,
}

/// The difference a store has **right now**, confirmed to be the one the caller was looking at.
///
/// Both halves are read fresh, and a failure to read either is a `503` rather than a degraded
/// answer. That is the difference between this and the single-store read: that read may show
/// "country not recorded" when a config load fails, because a partial page beats no page — but
/// *recording a decision* against a country this cloud could not actually read would put a fact in
/// the audit trail that nothing checked.
///
/// # Errors
///
/// The finished refusal: `404` for an unknown store, `503` when either half is unreadable, and
/// `FailedPrecondition` for a store with nothing to answer, a store whose codes have moved, or a
/// store whose region agrees.
async fn live_difference<F, Cfg>(
    fleet: &F,
    config_trees: &Cfg,
    tenant: TenantId,
    store: StoreId,
    claimed: (CountryCode, CountryCode),
) -> Result<RegionDifference, Response>
where
    F: FleetStore + Send + Sync,
    Cfg: ConfigTreeStore + Send + Sync,
{
    let (claimed_profile, claimed_region) = claimed;
    let row = match fleet.store_detail(tenant, store).await {
        Ok(Some(row)) => row,
        Ok(None) => return Err(not_found("store")),
        Err(error) => return Err(fleet_error_response(&error)),
    };
    let published = match config_trees.load(tenant, store).await {
        Ok(tree) => tree.and_then(|tree| published_country(&tree.record)),
        Err(error) => {
            tracing::error!(%error, "a store's country could not be read to record an acknowledgement");
            return Err(api_error(
                ErrorStatus::Unavailable,
                "the store's configuration could not be read; the acknowledgement was not recorded",
            ));
        }
    };
    let (Some(region), Some(published)) = (row.region.as_ref(), published) else {
        return Err(api_error(
            ErrorStatus::FailedPrecondition,
            "this store has no region difference to acknowledge — it has no region, or has not \
             published a country. Reload the store to see its current standing",
        ));
    };
    if region.country() != claimed_region || published != claimed_profile {
        return Err(api_error(
            ErrorStatus::FailedPrecondition,
            "the store has moved since this screen was drawn: the countries you named are not the \
             ones it has now. Reload it and look at the difference as it stands",
        ));
    }
    if region.country() == published {
        return Err(api_error(
            ErrorStatus::FailedPrecondition,
            "this store's region agrees with the country it is registered in, so there is nothing \
             to acknowledge",
        ));
    }
    Ok(RegionDifference {
        profile_country: published,
        region_country: region.country(),
    })
}

/// The two countries a store's live difference is between, confirmed against what the caller named.
struct RegionDifference {
    /// The country the store publishes in its `locale` node.
    profile_country: CountryCode,
    /// The country its data is resting in.
    region_country: CountryCode,
}

#[utoipa::path(
    post,
    path = "/admin/stores/{store_id}/region-acknowledgement",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.stores.manage"))),
    params(("store_id" = String, Path, description = "The store's 26-character ULID")),
    request_body(
        description = "The tenant, the two country codes the console showed, and one sentence \
                       saying why the difference between them is correct",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Recorded. The body is the answer as stored"),
        (status = 400, description = "A country code is not two letters, or the reason is blank \
                                      or over 280 characters", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.stores.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no such store", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "Nothing to answer, or the store has moved since this screen \
                                      was drawn — the codes you named are not the ones it has now", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The fleet, the config tree, or the acknowledgement store is \
                                      unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "fleet",
)]
/// An admin records why a store's region difference is correct
/// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// Behind [`ConsolePermission::ManageStores`] rather than `PublishConfig`: this is a judgement about
/// where a store's data may rest, which is the same judgement the bump that put it there was, and
/// the person who makes it is the person who moves stores between machines.
///
/// # It answers one difference, not the store
///
/// The request names both country codes and the write refuses `FailedPrecondition` when either has
/// moved since the console drew the screen. Without that check an admin could acknowledge a
/// difference that no longer exists — or worse, one that changed to something they never saw — and
/// the record would say a person approved a transfer they were never shown. The row then keeps the
/// pair, so the *read* applies the same rule for free: an answer stops standing when the store
/// moves, and the warning comes back unanswered.
///
/// Refused for a store with no difference at all, because there is nothing to answer: a store whose
/// region agrees, or which has no region, or which has published no country, would otherwise
/// accumulate reasons for questions nobody asked.
pub(super) async fn admin_acknowledge_region<F, A, C, Cfg, Ack>(
    State(state): State<FleetState<F, A, C, Cfg, Ack>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<AcknowledgeRegionRequest>,
) -> Response
where
    F: FleetStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    Ack: RegionAcknowledgementStore + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageStores,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant, store) =
        match parse_ulid_fields([("tenant_id", &request.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant, store]) => (TenantId::new(tenant), StoreId::new(store)),
            Err(refusal) => return refusal,
        };
    let claimed = match claimed_countries(&request) {
        Ok(claimed) => claimed,
        Err(refusal) => return refusal,
    };
    let difference =
        match live_difference(&state.fleet, &state.config_trees, tenant, store, claimed).await {
            Ok(difference) => difference,
            Err(refusal) => return refusal,
        };
    let acknowledgement = match answered_difference(&difference, &request.reason, state.clock.now())
    {
        Ok(acknowledgement) => acknowledgement,
        Err(refusal) => return refusal,
    };
    if let Err(refusal) =
        record_acknowledgement(&state, &context, tenant, store, &acknowledgement).await
    {
        return refusal;
    }
    (
        StatusCode::OK,
        Json(RegionAcknowledgementView::of(&acknowledgement)),
    )
        .into_response()
}

impl RegionAcknowledgementView {
    /// The answer as stored: both codes, the reason as typed, and when.
    fn of(acknowledgement: &RegionAcknowledgement) -> Self {
        Self {
            profile_country: acknowledgement.profile_country().as_str().to_owned(),
            region_country: acknowledgement.region_country().as_str().to_owned(),
            reason: acknowledgement.reason().to_owned(),
            acknowledged_at_ms: acknowledgement
                .acknowledged_at()
                .as_milliseconds_since_epoch(),
        }
    }
}

/// The two country codes the console showed, which say which difference is being answered.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the refusal the caller returns"
)]
fn claimed_countries(
    request: &AcknowledgeRegionRequest,
) -> Result<(CountryCode, CountryCode), Response> {
    let (Ok(claimed_profile), Ok(claimed_region)) = (
        CountryCode::parse(&request.profile_country),
        CountryCode::parse(&request.region_country),
    ) else {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "profile_country and region_country must both be ISO 3166-1 alpha-2 codes — the two \
             the console showed you. They say which difference you are answering",
            &[
                ("profile_country", "INVALID_FORMAT"),
                ("region_country", "INVALID_FORMAT"),
            ],
        ));
    };
    Ok((claimed_profile, claimed_region))
}

/// The answer to `difference`, once the reason is one sentence of at most 280 characters.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the refusal the caller returns"
)]
fn answered_difference(
    difference: &RegionDifference,
    reason: &str,
    now: Timestamp,
) -> Result<RegionAcknowledgement, Response> {
    RegionAcknowledgement::new(
        difference.profile_country,
        difference.region_country,
        reason,
        now,
    )
    .map_err(|refusal| {
        api_error_with_details(
            ErrorStatus::InvalidArgument,
            "reason is required and must be at most 280 characters — one sentence saying why \
             this store's data resting outside its own country is correct",
            &[("reason", refusal.code())],
        )
    })
}

/// Records the answer, then the trail entry carrying it.
async fn record_acknowledgement<F, A, C, Cfg, Ack>(
    state: &FleetState<F, A, C, Cfg, Ack>,
    context: &AdminContext,
    tenant: TenantId,
    store: StoreId,
    acknowledgement: &RegionAcknowledgement,
) -> Result<(), Response>
where
    C: ClockSource,
    Ack: RegionAcknowledgementStore,
{
    if let Err(error) = state
        .acknowledgements
        .record(tenant, store, acknowledgement)
        .await
    {
        tracing::error!(%error, "a region acknowledgement could not be recorded");
        return Err(api_error(
            ErrorStatus::Unavailable,
            "the acknowledgement could not be recorded",
        ));
    }
    // Unredacted, and deliberately: the two country codes are business metadata, and the reason is
    // operator-supplied free text carried whole because a trail that summarises the reason answers
    // nothing. It is never logged and never in an event payload.
    audit_action(
        &state.audit,
        &state.clock,
        context,
        Some(tenant),
        "store.edge_placement.acknowledge",
        "store",
        &store.to_string(),
        None,
        Some(serde_json::json!({
            "profile_country": acknowledgement.profile_country().as_str(),
            "region_country": acknowledgement.region_country().as_str(),
            "reason": acknowledgement.reason(),
        })),
    )
    .await;
    Ok(())
}

/// The country a store's published `locale` node names, or `None` when it has never published one
/// ([ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)).
///
/// Reads the store's layers composed, as it runs them ([`ConfigTreeState::effective`]):
/// `admin_publish_locale` writes the Store layer, and a rollback restores a version onto the Tenant
/// layer. A value that is not two ASCII letters answers `None` rather than an error: a node written
/// by a fork, or by a build older than the required-`country_code` change, is a store this console
/// still has to be able to draw.
pub(super) fn published_country(tree: &ConfigTreeState) -> Option<CountryCode> {
    tree.effective()
        .get("locale")?
        .get("country_code")?
        .as_str()
        .and_then(|code| CountryCode::parse(code).ok())
}
