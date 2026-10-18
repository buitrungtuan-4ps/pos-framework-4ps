// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The org registry (`/admin/tenants`, `/admin/brands`, `/admin/stores` and a store's devices,
//! ADR-0065).
//!
//! The state, the router, the request bodies and the store-error helpers are here.

use std::sync::Arc;

use axum::Router;
use axum::response::Response;
use axum::routing::get;
use brands::{admin_create_brand, admin_list_brands, admin_update_brand};
use devices::{admin_create_device, admin_list_devices, admin_update_device};
use pos_cloud_api::response::api_error;
use pos_cloud_config::route::registry_error_response;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use serde::Deserialize;
use stores::{admin_create_store, admin_list_stores, admin_update_store};
use tenants::{admin_create_tenant, admin_list_tenants, admin_update_tenant};

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::registry::RegistryStore;
use crate::starting_roles::StartingRoles;

mod brands;
mod devices;
mod stores;
mod tenants;

// --- Org registry (`/admin/tenants|brands|stores`, ADR-0065) ------------------------------------

/// The collaborators the registry routes need, stated independently of [`CloudApp`]: the registry
/// store, plus the admin and clock stores every route's session guard uses. Like [`device_router`],
/// it carries its own state and is merged into the main router rather than adding a `CloudApp` generic.
#[derive(Clone)]
struct RegistryState<Rg, A, C> {
    registry: Rg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    /// Gives a tenant created here its six starting roles
    /// ([ADR-0158](../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)).
    starting_roles: Arc<dyn StartingRoles>,
}

/// Builds the org-registry sub-router ([ADR-0065](../../../docs/adr/0065-cloud-org-registry.md)).
///
/// Every route is behind the super-admin session guard, and names a tenant the admin-is-global way
/// ([ADR-0060](../../../docs/adr/0060-cloud-back-office-dashboard.md)): a `?tenant_id=` query for the
/// listings, the request body for a create. Device routes nest under their store, so they never
/// collide with the `/admin/devices/proposals` onboarding queue ([`device_router`](crate::http::device_router)). A tenant created
/// here is given its six starting roles through `starting_roles`
/// ([`crate::starting_roles`]).
pub fn registry_router<Rg, A, C>(
    registry: Rg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    starting_roles: Arc<dyn StartingRoles>,
) -> Router
where
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/tenants",
            get(admin_list_tenants::<Rg, A, C>).post(admin_create_tenant::<Rg, A, C>),
        )
        .route(
            "/admin/tenants/{tenant_id}",
            axum::routing::patch(admin_update_tenant::<Rg, A, C>),
        )
        .route(
            "/admin/brands",
            get(admin_list_brands::<Rg, A, C>).post(admin_create_brand::<Rg, A, C>),
        )
        .route(
            "/admin/brands/{brand_id}",
            axum::routing::patch(admin_update_brand::<Rg, A, C>),
        )
        .route(
            "/admin/stores",
            get(admin_list_stores::<Rg, A, C>).post(admin_create_store::<Rg, A, C>),
        )
        .route(
            "/admin/stores/{store_id}",
            axum::routing::patch(admin_update_store::<Rg, A, C>),
        )
        .route(
            "/admin/stores/{store_id}/devices",
            get(admin_list_devices::<Rg, A, C>).post(admin_create_device::<Rg, A, C>),
        )
        .route(
            "/admin/stores/{store_id}/devices/{device_id}",
            axum::routing::patch(admin_update_device::<Rg, A, C>),
        )
        .with_state(RegistryState {
            registry,
            admin,
            clock,
            audit,
            starting_roles,
        })
}

#[derive(Debug, Clone, Deserialize)]
struct CreateTenantRequest {
    name: String,
    /// The language the tenant's starting roles are named in, a language tag such as `vi`. The
    /// console sends the one it is shown in. Absent, or one the console's language packs do not
    /// carry, names them in English. Nothing about the tenant records it.
    #[serde(default)]
    language: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateEntityRequest {
    name: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateBrandRequest {
    tenant_id: String,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateBrandRequest {
    tenant_id: String,
    name: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateStoreRequest {
    tenant_id: String,
    brand_id: Option<String>,
    name: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateStoreRequest {
    tenant_id: String,
    brand_id: Option<String>,
    name: String,
    status: String,
}

#[derive(Debug, Clone, Deserialize)]
struct CreateDeviceRequest {
    tenant_id: String,
    name: String,
    kind: String,
}

#[derive(Debug, Clone, Deserialize)]
struct UpdateDeviceRequest {
    tenant_id: String,
    name: String,
    kind: String,
    status: String,
}

/// The `503` returned when OS entropy is unavailable to mint an id.
fn registry_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy to mint a registry id");
    api_error(
        ErrorStatus::Unavailable,
        "the registry service is unavailable",
    )
}
