// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/fleet` and `/admin/fleet/{store_id}`: a tenant's stores, and one store.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::parse_ulid_fields;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use super::regions::published_country;
use super::views::FleetStoreView;
use super::{FleetState, RegionAcknowledgementView, fleet_error_response};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::fleet::FleetStore;
use crate::http::{Json, RegistryTenantQuery, not_found};
use crate::lease::{
    RegionAcknowledgementStore, RegionAgreement, region_agreement, standing_acknowledgement,
};

#[utoipa::path(
    get,
    path = "/admin/fleet",
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The tenant whose stores to list (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "Every store of the tenant, as an array, with its last contact, \
                                      `online` (derived at read time from the last contact), the \
                                      configuration version it holds against the one published and \
                                      `config_current`, the binary it runs and its last self-test, \
                                      its outbox depth, its print agents, its lease standing, and \
                                      where its edge runs. Instants are Unix milliseconds; a fact \
                                      the store never reported is `null`, never zero. The region \
                                      comparison is on the single-store read only"),
        (status = 400, description = "tenant_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The fleet store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "fleet",
)]
/// A super-admin (any console role) lists a tenant's whole fleet, online/offline derived at read.
pub(super) async fn admin_list_fleet<F, A, C, Cfg, Ack>(
    State(state): State<FleetState<F, A, C, Cfg, Ack>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    F: FleetStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    Ack: RegionAcknowledgementStore + Clone + Send + Sync + 'static,
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
    let tenant = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant]) => TenantId::new(tenant),
        Err(refusal) => return refusal,
    };
    let now_ms = state.clock.now().as_milliseconds_since_epoch();
    match state.fleet.list_fleet(tenant).await {
        Ok(rows) => {
            let views: Vec<FleetStoreView> = rows
                .into_iter()
                .map(|row| FleetStoreView::from_row(row, now_ms))
                .collect();
            (StatusCode::OK, Json(views)).into_response()
        }
        Err(error) => fleet_error_response(&error),
    }
}

#[utoipa::path(
    get,
    path = "/admin/fleet/{store_id}",
    security(("session_cookie" = [])),
    params(
        ("store_id" = String, Path, description = "The store (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The store's tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The store as the fleet list shows it, plus the region \
                                      comparison (ADR-0114): `profile_country`, the country its \
                                      configuration publishes; `region_agreement`, `agrees`, \
                                      `differs` or `country-not-recorded`; and \
                                      `region_acknowledgement`, the recorded answer to a difference, \
                                      if one stands. If the store's configuration cannot be read, \
                                      the comparison degrades to no country rather than failing the \
                                      read"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.read", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no such store", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The fleet store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "fleet",
)]
/// A super-admin (any console role) reads one store's fleet detail; `404` if the tenant has no such
/// store.
pub(super) async fn admin_fleet_store<F, A, C, Cfg, Ack>(
    State(state): State<FleetState<F, A, C, Cfg, Ack>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    F: FleetStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    Ack: RegionAcknowledgementStore + Clone + Send + Sync + 'static,
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
    let now_ms = state.clock.now().as_milliseconds_since_epoch();
    let row = match state.fleet.store_detail(tenant, store).await {
        Ok(Some(row)) => row,
        Ok(None) => return not_found("store"),
        Err(error) => return fleet_error_response(&error),
    };
    // The country this store publishes, for the one comparison ADR-0114 asks for. One `load` for
    // the one store already named — the read this route performs anyway is a single row, so this
    // is bounded work, unlike the same comparison on the listing.
    //
    // A config-tree failure is **not** an error here. This read's job is to report a store's
    // health, and failing the whole page because a second store's configuration could not be read
    // would take the console away from an operator mid-incident. It degrades to "no country", which
    // renders as `country-not-recorded` — visibly not an answer, never as agreement.
    let profile_country = match state.config_trees.load(tenant, store).await {
        Ok(tree) => tree.and_then(|tree| published_country(&tree.record)),
        Err(error) => {
            tracing::warn!(%error, "a store's country could not be read for its region comparison");
            None
        }
    };
    let agreement = region_agreement(row.region.as_ref(), profile_country);
    // The answer, if somebody has given one for *this* difference. A read failure degrades the same
    // way the tree's does and for the same reason — a store that has been answered showing as
    // unanswered is a warning too many, which is the safe direction to fail in.
    let recorded = match state.acknowledgements.current(tenant, store).await {
        Ok(recorded) => recorded,
        Err(error) => {
            tracing::warn!(%error, "a store's region acknowledgement could not be read");
            None
        }
    };
    let standing =
        standing_acknowledgement(recorded.as_ref(), row.region.as_ref(), profile_country).map(
            |acknowledgement| RegionAcknowledgementView {
                profile_country: acknowledgement.profile_country().as_str().to_owned(),
                region_country: acknowledgement.region_country().as_str().to_owned(),
                reason: acknowledgement.reason().to_owned(),
                acknowledged_at_ms: acknowledgement
                    .acknowledged_at()
                    .as_milliseconds_since_epoch(),
            },
        );
    let mut view = FleetStoreView::from_row(row, now_ms);
    view.profile_country = profile_country.map(|country| country.as_str().to_owned());
    view.region_agreement = agreement.map(RegionAgreement::as_wire);
    view.region_acknowledgement = standing;
    (StatusCode::OK, Json(view)).into_response()
}
