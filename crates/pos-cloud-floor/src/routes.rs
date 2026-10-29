// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The floor and kitchen: the areas, tables, stations and routing rules a store is authored with
//! under `/admin/floor` and `/admin/kitchen`, their publish, and table QR tokens (ADR-0072).
//!
//! The state, the router, and the tenant and refusal helpers the handlers share are here.

use std::sync::Arc;

use areas::{admin_create_area, admin_get_area, admin_list_areas, admin_update_area};
use axum::Router;
use axum::response::Response;
use axum::routing::{delete, get};
use pos_cloud_api::response::{api_error, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use requests::FloorListQuery;
use routing::{admin_create_routing, admin_list_routing, admin_remove_routing};
use stations::{
    admin_create_station, admin_get_station, admin_list_stations, admin_update_station,
};
use tables::{admin_create_table, admin_get_table, admin_list_tables, admin_update_table};

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::catalog::CatalogStore;
use crate::floorplan::{AreaStore, RoutingRuleStore, StationStore, TableStore};

mod areas;
mod publish;
mod requests;
mod routing;
mod stations;
mod table_qr;
mod tables;

pub use publish::floor_publish_router;
// What the batch fan-out uses.
pub use publish::floor_nodes;
pub use table_qr::table_qr_router;
// The handlers the console's OpenAPI document names.
pub use areas::{
    __path_admin_create_area, __path_admin_get_area, __path_admin_list_areas,
    __path_admin_update_area,
};
pub use publish::__path_admin_publish_floor;
pub use routing::{
    __path_admin_create_routing, __path_admin_list_routing, __path_admin_remove_routing,
};
pub use stations::{
    __path_admin_create_station, __path_admin_get_station, __path_admin_list_stations,
    __path_admin_update_station,
};
pub use table_qr::__path_admin_list_table_qr;
pub use tables::{
    __path_admin_create_table, __path_admin_get_table, __path_admin_list_tables,
    __path_admin_update_table,
};

// --- Floor & kitchen master data (`/admin/floor`, `/admin/kitchen`, ADR-0072) -------------------

/// The collaborators the floor/kitchen CRUD routes need: the master-data store, plus the
/// admin/clock/audit every write carries.
#[derive(Clone)]
struct FloorState<F, A, C, Cat> {
    floor: F,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    /// The tenant's catalog, for the one thing a floor write has to ask it: does this course exist?
    ///
    /// A routing rule matching on a course was accepted after checking only that it did not also
    /// name an item, because there was nowhere to look ([ADR-0130](../../../docs/adr/0130-a-course-is-something-the-catalog-names.md)
    /// decision 6). Now there is, and the rule is refused at the write the way the menu graph's
    /// cycle is — a rule that can never match is a rule an operator will never find out about.
    catalog: Cat,
}

/// Maps any floor-store failure to a retryable `503`, logging the detail rather than leaking it.
fn floor_error_response(error: &impl std::fmt::Display) -> Response {
    tracing::error!(%error, "a floor & kitchen store operation failed");
    api_error(ErrorStatus::Unavailable, "the floor service is unavailable")
}

/// `503` when OS entropy is unavailable to mint a floor/kitchen id.
fn floor_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy for a floor & kitchen write");
    api_error(ErrorStatus::Unavailable, "the floor service is unavailable")
}

/// Builds the floor & kitchen master-data sub-router ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)).
///
/// Reads are behind [`ConsolePermission::Read`](crate::auth::console_rbac::ConsolePermission::Read); every write is behind [`ConsolePermission::ManageFloor`](crate::auth::console_rbac::ConsolePermission::ManageFloor)
/// (Owner/Admin) and is audited. The tenant is named the admin-is-global way (a `?tenant_id=` /
/// `?store_id=` query on reads, the request body on writes). None of this data is PII.
pub fn floor_router<F, A, C, Cat>(
    floor: F,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    catalog: Cat,
) -> Router
where
    F: AreaStore + TableStore + StationStore + RoutingRuleStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/floor/areas",
            get(admin_list_areas::<F, A, C, Cat>).post(admin_create_area::<F, A, C, Cat>),
        )
        .route(
            "/admin/floor/areas/{area_id}",
            get(admin_get_area::<F, A, C, Cat>).patch(admin_update_area::<F, A, C, Cat>),
        )
        .route(
            "/admin/floor/tables",
            get(admin_list_tables::<F, A, C, Cat>).post(admin_create_table::<F, A, C, Cat>),
        )
        .route(
            "/admin/floor/tables/{table_id}",
            get(admin_get_table::<F, A, C, Cat>).patch(admin_update_table::<F, A, C, Cat>),
        )
        .route(
            "/admin/kitchen/stations",
            get(admin_list_stations::<F, A, C, Cat>).post(admin_create_station::<F, A, C, Cat>),
        )
        .route(
            "/admin/kitchen/stations/{station_id}",
            get(admin_get_station::<F, A, C, Cat>).patch(admin_update_station::<F, A, C, Cat>),
        )
        .route(
            "/admin/kitchen/routing",
            get(admin_list_routing::<F, A, C, Cat>).post(admin_create_routing::<F, A, C, Cat>),
        )
        .route(
            "/admin/kitchen/routing/{rule_id}",
            delete(admin_remove_routing::<F, A, C, Cat>),
        )
        .with_state(FloorState {
            floor,
            admin,
            clock,
            audit,
            catalog,
        })
}

/// Reads a `?tenant_id=` query into a `TenantId`, or returns the `400`.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the shared 400 these route helpers return"
)]
fn floor_tenant(tenant_id: &str) -> Result<TenantId, Response> {
    let [tenant] = parse_ulid_fields([("tenant_id", tenant_id)])?;
    Ok(TenantId::new(tenant))
}

/// Reads a (tenant, store) list query, or returns the `400`.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — the shared 400 these route helpers return"
)]
fn floor_tenant_store(query: &FloorListQuery) -> Result<(TenantId, StoreId), Response> {
    let [tenant, store] = parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("store_id", &query.store_id),
    ])?;
    Ok((TenantId::new(tenant), StoreId::new(store)))
}
