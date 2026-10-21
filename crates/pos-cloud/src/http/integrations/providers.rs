// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/integrations/providers`: the vendors this cloud can talk to (ADR-0153).

use std::sync::Arc;

use axum::Json;
use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pos_cloud_api::console::require_permission;
use pos_proto::determinism::ClockSource;

use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;

// --- The vendor catalogue (`/admin/integrations/providers`, ADR-0153) ----------------------------

/// The collaborators the provider-catalogue read needs: the registry built at boot, and the admin
/// and clock its session guard uses.
#[derive(Clone)]
struct ProviderCatalogueState<A, C> {
    providers: Arc<pos_providers::Registry>,
    admin: A,
    clock: C,
}

/// Builds the provider-catalogue sub-router
/// ([ADR-0153](../../../docs/adr/0153-a-vendor-is-a-provider-the-cloud-chooses.md)).
///
/// One read, behind [`ConsolePermission::Read`]: every vendor this binary can talk to, with the
/// settings schema the console draws a connection form from. Tenant-independent, like the capability
/// catalogue — it describes the platform, not anybody's configuration.
pub fn provider_catalogue_router<A, C>(
    providers: Arc<pos_providers::Registry>,
    admin: A,
    clock: C,
) -> Router
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/integrations/providers",
            get(admin_list_providers::<A, C>),
        )
        .with_state(ProviderCatalogueState {
            providers,
            admin,
            clock,
        })
}

#[utoipa::path(
    get,
    path = "/admin/integrations/providers",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    responses(
        (status = 200, description = "Every provider compiled into this cloud, grouped by family: \
                                      its id, where its adapter runs, the countries it serves, \
                                      whether it is a sandbox, and the typed fields a connection \
                                      to it needs. A secret field is marked as one and has no \
                                      value here or anywhere else the API answers"),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "integrations",
)]
/// Serves the provider catalogue for the console's Integrations screen.
async fn admin_list_providers<A, C>(
    State(state): State<ProviderCatalogueState<A, C>>,
    headers: HeaderMap,
) -> Response
where
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
    (
        StatusCode::OK,
        Json(crate::providers::catalogue(&state.providers)),
    )
        .into_response()
}
