// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /admin/floor/publish`: a store's floor and kitchen compiled into its `floor` and
//! `stations` nodes.

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{parse_ulid_fields, service_unavailable};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use serde::Deserialize;

use super::floor_error_response;
use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::floor_compiler::{compile_floor, compile_stations};
use crate::floorplan::{
    Area, AreaStore, RoutingRuleStore, Station, StationStore, Table, TableStore,
};
use axum::Json;
use pos_cloud_api::refusal::unprocessable_violations;
use pos_cloud_config::publish::publish_config_nodes;
use pos_cloud_config::route::PublishedConfig;

// --- Floor & kitchen publish (`/admin/floor/publish`, ADR-0072) ---------------------------------

/// The collaborators the floor/kitchen publish route needs: the master-data store to compile from, the
/// config-tree store to write onto, plus the admin/clock/audit every write carries.
#[derive(Clone)]
struct FloorPublishState<F, Cfg, A, C> {
    floor: F,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A super-admin selects the (tenant, store) whose `floor`/`stations` nodes to compile and publish.
#[derive(Debug, Clone, Deserialize)]
struct PublishFloorRequest {
    tenant_id: String,
    store_id: String,
}

/// Builds the floor & kitchen publish sub-router ([ADR-0072](../../../docs/adr/0072-floor-and-kitchen.md)).
///
/// One route: compile a store's areas/tables into a `FloorPlan` and its stations/routing into a
/// `StationPlan`, run the §10 referential validation (`pos_core::floor`), and — only if valid — merge
/// both onto the store's `floor` and `stations` config nodes and version them through the config tree.
/// Behind [`ConsolePermission::PublishConfig`], the same gate as catalog/people/config publish.
pub fn floor_publish_router<F, Cfg, A, C>(
    floor: F,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    F: AreaStore + TableStore + StationStore + RoutingRuleStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/floor/publish",
            post(admin_publish_floor::<F, Cfg, A, C>),
        )
        .with_state(FloorPublishState {
            floor,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// How big the plan a floor publish compiled was — for the audit trail, never the plan itself.
#[derive(Debug, Clone, Copy, serde::Serialize)]
pub struct FloorPublishSummary {
    areas: usize,
    tables: usize,
    stations: usize,
}

/// Compiles one store's areas, tables, stations and routing into its `floor` and `stations` nodes.
///
/// Per store, like `pos-cloud`'s `permissions_nodes` and for the same reason: every shop's plan is
/// its own. A batch ([ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md) §4)
/// calls this once per member — the instruction "recompile each of these shops' own floors"
/// carries nothing store-specific even though every result does.
///
/// The §10 referential validation runs before anything is written, so the cloud never publishes a
/// plan that names a station, backup, or area that does not exist.
///
/// # Errors
///
/// The response to answer with: `503` when the floor store fails or a node does not serialise,
/// `422` when the plan fails the referential validation.
pub async fn floor_nodes<F>(
    floor: &F,
    tenant_id: TenantId,
    store_id: StoreId,
) -> Result<(Vec<(String, serde_json::Value)>, FloorPublishSummary), Response>
where
    F: AreaStore + TableStore + StationStore + RoutingRuleStore,
{
    // Load the authoring rows. `list` is on all four seams, so each call is fully-qualified. The
    // version each row was read at is a writer's concern (ADR-0094); a compiled plan carries the
    // floor, not the console's edit history, so the versions are dropped here.
    let areas: Vec<Area> = match AreaStore::list(floor, tenant_id, store_id).await {
        Ok(areas) => areas.into_iter().map(|area| area.record).collect(),
        Err(error) => return Err(floor_error_response(&error)),
    };
    let tables: Vec<Table> = match TableStore::list(floor, tenant_id, store_id).await {
        Ok(tables) => tables.into_iter().map(|table| table.record).collect(),
        Err(error) => return Err(floor_error_response(&error)),
    };
    let stations: Vec<Station> = match StationStore::list(floor, tenant_id, store_id).await {
        Ok(stations) => stations.into_iter().map(|station| station.record).collect(),
        Err(error) => return Err(floor_error_response(&error)),
    };
    let rules = match RoutingRuleStore::list(floor, tenant_id, store_id).await {
        Ok(rules) => rules,
        Err(error) => return Err(floor_error_response(&error)),
    };

    let floor_plan = compile_floor(&areas, &tables);
    let station_plan = compile_stations(&stations, &rules);

    let mut violations = pos_core::floor::floor_violations(&floor_plan);
    violations.extend(pos_core::floor::station_violations(&station_plan));
    if !violations.is_empty() {
        return Err(unprocessable_violations(&violations));
    }

    let summary = FloorPublishSummary {
        areas: floor_plan.areas().len(),
        tables: floor_plan.tables().count(),
        stations: station_plan.stations().len(),
    };
    let (Ok(floor_value), Ok(stations_value)) = (
        serde_json::to_value(&floor_plan),
        serde_json::to_value(&station_plan),
    ) else {
        tracing::error!("could not serialise a compiled floor or station plan");
        return Err(service_unavailable("floor"));
    };
    Ok((
        vec![
            ("floor".to_owned(), floor_value),
            ("stations".to_owned(), stations_value),
        ],
        summary,
    ))
}

#[utoipa::path(
    post,
    path = "/admin/floor/publish",
    extensions(("x-permission" = json!("console.config.publish"))),
    security(("session_cookie" = [])),
    request_body(
        description = "`tenant_id` and `store_id`. What is published is the store's authored floor and \
                       kitchen, never the body",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "The store's areas and tables are compiled into its `floor` node, \
                                      and its stations and routing into its `stations` node, and both \
                                      are published on the Store layer, the store's other settings \
                                      kept. The body carries `config_version_id`. Audited with the \
                                      plan's size, not the plan"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID, or a value needs \
                                      a recorded confirmation first", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "Every attempt to publish lost a race with another publish: \
                                      try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 422, description = "The plan breaks a floor or kitchen rule, such as a routing rule \
                                      to an unknown station or a stale backup, with every violation \
                                      listed; or the store's configuration would not validate", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The floor store or the configuration store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "floor",
)]
/// Compiles a store's floor & kitchen master data into its `floor`/`stations` config nodes and versions
/// them — the same load→compile→validate→write→version shape as `admin_publish_menu`, onto the
/// `floor`/`stations` keys. The §10 referential rules run here (a rule to an unknown station, a stale
/// backup): an invalid plan is a `422` with the violated rules, never a stored state.
async fn admin_publish_floor<F, Cfg, A, C>(
    State(state): State<FloorPublishState<F, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishFloorRequest>,
) -> Response
where
    F: AreaStore + TableStore + StationStore + RoutingRuleStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::PublishConfig,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, store_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
    ]) {
        Ok([tenant_id, store_id]) => (TenantId::new(tenant_id), StoreId::new(store_id)),
        Err(refusal) => return refusal,
    };

    // Set the `floor` and `stations` keys on the store's Store layer (index 2) and re-publish it,
    // preserving the other Store-level keys (`menu`, `layout`, `permissions`, capability flags).
    let (nodes, summary) = match floor_nodes(&state.floor, tenant_id, store_id).await {
        Ok(compiled) => compiled,
        Err(refusal) => return refusal,
    };

    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    {
        audit_action(
            &state.audit,
            &state.clock,
            &context,
            Some(tenant_id),
            "floor.publish",
            "store",
            &store_id.to_string(),
            None,
            Some(serde_json::json!({
                "config_version_id": id.to_string(),
                "area_count": summary.areas,
                "table_count": summary.tables,
                "station_count": summary.stations,
            })),
        )
        .await;
        (
            StatusCode::OK,
            Json(PublishedConfig {
                config_version_id: id.to_string(),
            }),
        )
            .into_response()
    }
}
