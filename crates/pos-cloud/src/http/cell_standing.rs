// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A store whose country differs from its cell's: the warning and its audited acknowledgement
//! ([ADR-0179](../../../docs/adr/0179-cells-the-directory-and-the-front-door.md) C6).
//!
//! A cell has a home country (`[cell] home_country`); a store publishes its own in its `locale` node.
//! When the two differ, the store's data rests in a cell homed elsewhere. Whether that is lawful
//! depends on the basis for the transfer, which the operator holds and the code cannot know, so this
//! **never refuses** anything: it warns, and lets a person with `console.stores.manage` say why the
//! difference is correct, exactly as [ADR-0114](../../../docs/adr/0114-region-is-required-recorded-visible.md)
//! does for a hosted store's region.
//!
//! # The trail is the record
//!
//! An acknowledgement is one audit entry, `store.cell_country.acknowledge`, carrying the two countries
//! it was about and the reason. The standing read takes the newest such entry for the store, and it
//! counts only while its pair is still the store's pair: a reason written about VN-in-a-JP-cell says
//! nothing once the store publishes another country, so the warning comes back unanswered. Because
//! the entry *is* the answer, it is written directly and its failure refuses the request, unlike the
//! best-effort entries that only describe a change made elsewhere. No table holds a copy.

use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use pos_cloud_api::console::{ConfigTenantQuery, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error, api_error_with_details, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::locale::CountryCode;
use serde::Deserialize;

use crate::audit::{AuditActor, AuditEntry, AuditId, AuditQuery, AuditStore};
use crate::auth::admin::{AdminContext, AdminStore};
use crate::auth::console_rbac::ConsolePermission;
use crate::cell::Cell;
use crate::config_tree::{ConfigTreeState, ConfigTreeStore};
use crate::http::Json;
use crate::lease::{REGION_REASON_LIMIT, RegionReasonRefusal};

/// The audit action an acknowledgement is recorded as, and read back by.
pub(crate) const ACKNOWLEDGE_ACTION: &str = "store.cell_country.acknowledge";

/// A store's country against its cell's.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct CellStandingView {
    /// The cell this store's data rests in.
    cell_id: String,
    /// The cell's home country. Null when the cell has no `[cell]` section, which compares with
    /// nothing.
    #[schema(required = true)]
    cell_country: Option<String>,
    /// The country the store publishes in its `locale` node. Null when it has published none.
    #[schema(required = true)]
    profile_country: Option<String>,
    /// Whether both are known and differ. A warning, never a refusal.
    differs: bool,
    /// The answer to this difference, while it is still this difference.
    #[schema(required = true)]
    acknowledgement: Option<CellAcknowledgementView>,
}

/// A recorded answer to a store's cell difference.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, utoipa::ToSchema)]
pub(crate) struct CellAcknowledgementView {
    /// The store's country the answer was about.
    profile_country: String,
    /// The cell's country the answer was about.
    cell_country: String,
    /// Why the difference is correct, as typed.
    reason: String,
    /// When it was recorded, in milliseconds since the epoch.
    acknowledged_at_ms: i64,
}

/// What a console sends to answer a store's cell warning. The two countries say which difference is
/// answered: a write naming a pair the store no longer has is refused.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
pub(crate) struct AcknowledgeCellRequest {
    /// The tenant the store belongs to.
    tenant_id: String,
    /// The store's country the console showed.
    profile_country: String,
    /// The cell's country the console showed.
    cell_country: String,
    /// Why the difference is correct: required, at most 280 characters. Defaulted so an omitted
    /// field is refused through the envelope naming it.
    #[serde(default)]
    reason: String,
}

#[derive(Clone)]
struct CellStandingState<T, Cfg, A, C> {
    trail: T,
    config_trees: Cfg,
    admin: A,
    clock: C,
    cell: Arc<Cell>,
}

/// Builds the store cell-standing sub-router: the read behind `console.data.read`, the answer behind
/// `console.stores.manage`.
pub fn cell_standing_router<T, Cfg, A, C>(
    trail: T,
    config_trees: Cfg,
    admin: A,
    clock: C,
    cell: Arc<Cell>,
) -> Router
where
    T: AuditStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/stores/{store_id}/cell-standing",
            get(admin_cell_standing::<T, Cfg, A, C>),
        )
        .route(
            "/admin/stores/{store_id}/cell-acknowledgement",
            post(admin_acknowledge_cell::<T, Cfg, A, C>),
        )
        .with_state(CellStandingState {
            trail,
            config_trees,
            admin,
            clock,
            cell,
        })
}

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/cell-standing",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    params(
        ("store_id" = String, Path, description = "The store's 26-character ULID"),
        ("tenant_id" = String, Query, description = "The tenant the store belongs to"),
    ),
    responses(
        (status = 200, body = CellStandingView, description = "The store's country against its \
                                      cell's: `cell_id`, `cell_country`, `profile_country`, \
                                      whether they `differ`, and the `acknowledgement` that \
                                      answers this difference, if one does (ADR-0179 C6)"),
        (status = 400, description = "tenant_id or store_id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The config tree or the audit trail is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "cell",
)]
/// A console admin reads whether a store's country differs from its cell's, and how it is answered.
async fn admin_cell_standing<T, Cfg, A, C>(
    State(state): State<CellStandingState<T, Cfg, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<ConfigTenantQuery>,
) -> Response
where
    T: AuditStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
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
    let (tenant, store) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant, store]) => (TenantId::new(tenant), StoreId::new(store)),
            Err(refusal) => return refusal,
        };
    let profile = match store_country(&state.config_trees, tenant, store).await {
        Ok(profile) => profile,
        Err(refusal) => return refusal,
    };
    let latest = match latest_acknowledgement(&state.trail, tenant, store).await {
        Ok(latest) => latest,
        Err(refusal) => return refusal,
    };
    let view = standing(&state.cell, profile, latest);
    (StatusCode::OK, Json(view)).into_response()
}

#[utoipa::path(
    post,
    path = "/admin/stores/{store_id}/cell-acknowledgement",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.stores.manage"))),
    params(("store_id" = String, Path, description = "The store's 26-character ULID")),
    request_body(
        content = AcknowledgeCellRequest, description = "The tenant, the two country codes the console showed, and one sentence \
                       saying why the difference between them is correct",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = CellAcknowledgementView, description = "Recorded in the audit trail. The body is the answer as recorded"),
        (status = 400, description = "An id is not a ULID, a country code is not two letters, or \
                                      the reason is blank or over 280 characters", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.stores.manage", body = crate::openapi_admin::ErrorResponse),
        (status = 409, description = "Nothing to answer, or the store or cell has moved since this \
                                      screen was drawn: the codes named are not the ones it has now", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The config tree or the audit trail is unreachable; nothing \
                                      was recorded", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "cell",
)]
/// An admin records why a store's country differing from its cell's is correct.
async fn admin_acknowledge_cell<T, Cfg, A, C>(
    State(state): State<CellStandingState<T, Cfg, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Json(request): Json<AcknowledgeCellRequest>,
) -> Response
where
    T: AuditStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
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
    let (Ok(claimed_profile), Ok(claimed_cell)) = (
        CountryCode::parse(&request.profile_country),
        CountryCode::parse(&request.cell_country),
    ) else {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "profile_country and cell_country must both be ISO 3166-1 alpha-2 codes: the two the \
             console showed you. They say which difference you are answering",
            &[
                ("profile_country", "INVALID_FORMAT"),
                ("cell_country", "INVALID_FORMAT"),
            ],
        );
    };
    let reason = match checked_reason(&request.reason) {
        Ok(reason) => reason,
        Err(refusal) => {
            return api_error_with_details(
                ErrorStatus::InvalidArgument,
                "reason is required and must be at most 280 characters: one sentence saying why \
                 this store's data resting in a cell homed in another country is correct",
                &[("reason", refusal.code())],
            );
        }
    };
    let profile = match store_country(&state.config_trees, tenant, store).await {
        Ok(profile) => profile,
        Err(refusal) => return refusal,
    };
    if let Err(why) = live_difference(
        state.cell.home_country,
        profile,
        (claimed_profile, claimed_cell),
    ) {
        return api_error(ErrorStatus::FailedPrecondition, why);
    }
    let now = state.clock.now();
    let answer = CellAcknowledgementView {
        profile_country: claimed_profile.as_str().to_owned(),
        cell_country: claimed_cell.as_str().to_owned(),
        reason: reason.to_owned(),
        acknowledged_at_ms: now.as_milliseconds_since_epoch(),
    };
    let Some(entry) = acknowledgement_entry(&context, tenant, store, &answer, now) else {
        return api_error(
            ErrorStatus::Unavailable,
            "the acknowledgement could not be given an id; nothing was recorded",
        );
    };
    if let Err(error) = state.trail.append(&entry).await {
        tracing::error!(%error, "a cell acknowledgement could not be recorded");
        return api_error(
            ErrorStatus::Unavailable,
            "the acknowledgement could not be recorded",
        );
    }
    (StatusCode::OK, Json(answer)).into_response()
}

/// The store's published country. `None` for a store with no tree or no `locale` country.
async fn store_country<Cfg>(
    config_trees: &Cfg,
    tenant: TenantId,
    store: StoreId,
) -> Result<Option<CountryCode>, Response>
where
    Cfg: ConfigTreeStore + Send + Sync,
{
    match config_trees.load(tenant, store).await {
        Ok(tree) => Ok(tree.and_then(|tree| published_country(&tree.record))),
        Err(error) => {
            tracing::error!(%error, "a store's country could not be read for its cell standing");
            Err(api_error(
                ErrorStatus::Unavailable,
                "the store's configuration could not be read",
            ))
        }
    }
}

/// The newest acknowledgement recorded for the store, whatever pair it was about.
async fn latest_acknowledgement<T>(
    trail: &T,
    tenant: TenantId,
    store: StoreId,
) -> Result<Option<CellAcknowledgementView>, Response>
where
    T: AuditStore + Send + Sync,
{
    let filter = AuditQuery {
        tenant: Some(tenant),
        entity_type: Some("store".to_owned()),
        entity_id: Some(store.to_string()),
        action: Some(ACKNOWLEDGE_ACTION.to_owned()),
        ..AuditQuery::default()
    };
    match trail.query(&filter, 1).await {
        Ok(entries) => Ok(entries.first().and_then(acknowledgement_of)),
        Err(error) => {
            tracing::error!(%error, "a store's cell acknowledgement could not be read");
            Err(api_error(
                ErrorStatus::Unavailable,
                "the audit trail could not be read",
            ))
        }
    }
}

/// The standing: whether the two countries differ, and the answer when it still answers them.
fn standing(
    cell: &Cell,
    profile: Option<CountryCode>,
    latest: Option<CellAcknowledgementView>,
) -> CellStandingView {
    let differs = matches!((cell.home_country, profile), (Some(home), Some(own)) if home != own);
    let acknowledgement = latest.filter(|answer| {
        differs
            && Some(answer.profile_country.as_str()) == profile.as_ref().map(CountryCode::as_str)
            && Some(answer.cell_country.as_str())
                == cell.home_country.as_ref().map(CountryCode::as_str)
    });
    CellStandingView {
        cell_id: cell.cell_id.clone(),
        cell_country: cell.home_country.map(|code| code.as_str().to_owned()),
        profile_country: profile.map(|code| code.as_str().to_owned()),
        differs,
        acknowledgement,
    }
}

/// Whether the store has, right now, the difference the caller named.
fn live_difference(
    cell_country: Option<CountryCode>,
    profile: Option<CountryCode>,
    claimed: (CountryCode, CountryCode),
) -> Result<(), &'static str> {
    let (Some(cell_country), Some(profile)) = (cell_country, profile) else {
        return Err(
            "there is no difference to acknowledge: this cell has no home country, or the store \
             has published no country. Reload the store to see its standing",
        );
    };
    if (profile, cell_country) != claimed {
        return Err(
            "the store or the cell has moved since this screen was drawn: the countries you named \
             are not the ones it has now. Reload it and look at the difference as it stands",
        );
    }
    if profile == cell_country {
        return Err(
            "this store's country is its cell's home country, so there is nothing to acknowledge",
        );
    }
    Ok(())
}

/// The reason, trimmed, when it is one sentence of at most 280 characters.
fn checked_reason(reason: &str) -> Result<&str, RegionReasonRefusal> {
    let reason = reason.trim();
    if reason.is_empty() {
        return Err(RegionReasonRefusal::Empty);
    }
    if reason.chars().count() > REGION_REASON_LIMIT {
        return Err(RegionReasonRefusal::TooLong);
    }
    Ok(reason)
}

/// The trail entry that is the answer. `None` only when no id can be minted.
fn acknowledgement_entry(
    context: &AdminContext,
    tenant: TenantId,
    store: StoreId,
    answer: &CellAcknowledgementView,
    now: pos_proto::time::Timestamp,
) -> Option<AuditEntry> {
    let ulid = mint_ulid(now.as_milliseconds_since_epoch())?;
    Some(AuditEntry {
        id: AuditId::new(ulid),
        tenant_id: Some(tenant),
        actor: AuditActor {
            admin_id: context.admin.id.clone(),
            email: context.admin.email.clone(),
            role: context.admin.role,
        },
        action: ACKNOWLEDGE_ACTION.to_owned(),
        entity_type: "store".to_owned(),
        entity_id: store.to_string(),
        before: None,
        after: Some(serde_json::json!({
            "profile_country": answer.profile_country,
            "cell_country": answer.cell_country,
            "reason": answer.reason,
        })),
        request_id: None,
        at: now,
    })
}

/// The answer a trail entry records, or `None` for an entry this build cannot read.
fn acknowledgement_of(entry: &AuditEntry) -> Option<CellAcknowledgementView> {
    let after = entry.after.as_ref()?;
    Some(CellAcknowledgementView {
        profile_country: after.get("profile_country")?.as_str()?.to_owned(),
        cell_country: after.get("cell_country")?.as_str()?.to_owned(),
        reason: after.get("reason")?.as_str()?.to_owned(),
        acknowledged_at_ms: entry.at.as_milliseconds_since_epoch(),
    })
}

/// The country a store's published `locale` node names, read from its layers composed. The same
/// reading the region warning makes (`http::fleet::regions`); kept beside its second use rather
/// than shared until a third needs it.
fn published_country(tree: &ConfigTreeState) -> Option<CountryCode> {
    tree.effective()
        .get("locale")?
        .get("country_code")?
        .as_str()
        .and_then(|code| CountryCode::parse(code).ok())
}

#[cfg(test)]
mod tests;
