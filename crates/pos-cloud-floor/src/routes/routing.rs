// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/kitchen/routing`: list, create and remove the rules that route items to stations.

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields, ulid_refusal};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{CourseId, MenuItemId, StationId, StoreId, TenantId};

use super::requests::{CreateRoutingRuleRequest, FloorListQuery, FloorTenantQuery};
use super::{FloorState, floor_entropy_unavailable, floor_error_response, floor_tenant_store};
use crate::auth::admin::{AdminContext, AdminStore};
use crate::auth::console_rbac::ConsolePermission;
use crate::catalog::CatalogStore;
use crate::floorplan::{NewRoutingRule, RoutingRule, RoutingRuleId, RoutingRuleStore};
use crate::registry::EntityStatus;
use axum::Json;
use pos_cloud_api::refusal::{not_found, parse_optional_ulid};
use pos_cloud_catalog::routes::catalog::catalog_error_response;

pub(super) async fn admin_list_routing<F, A, C, Cat>(
    State(state): State<FloorState<F, A, C, Cat>>,
    headers: HeaderMap,
    Query(query): Query<FloorListQuery>,
) -> Response
where
    F: RoutingRuleStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
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
    let (tenant_id, store_id) = match floor_tenant_store(&query) {
        Ok(scope) => scope,
        Err(response) => return response,
    };
    match RoutingRuleStore::list(&state.floor, tenant_id, store_id).await {
        Ok(rules) => (StatusCode::OK, Json::<Vec<RoutingRule>>(rules)).into_response(),
        Err(error) => floor_error_response(&error),
    }
}

pub(super) async fn admin_create_routing<F, A, C, Cat>(
    State(state): State<FloorState<F, A, C, Cat>>,
    headers: HeaderMap,
    Json(request): Json<CreateRoutingRuleRequest>,
) -> Response
where
    F: RoutingRuleStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageFloor,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, store_id, station_id) = match parse_ulid_fields([
        ("tenant_id", &request.tenant_id),
        ("store_id", &request.store_id),
        ("station_id", &request.station_id),
    ]) {
        Ok([tenant_id, store_id, station_id]) => (
            TenantId::new(tenant_id),
            StoreId::new(store_id),
            StationId::new(station_id),
        ),
        Err(refusal) => return refusal,
    };
    let (menu_item_id, course_id) = match routing_target(&request) {
        Ok(target) => target,
        Err(refusal) => return refusal,
    };
    if let Some(course_id) = course_id
        && let Err(refusal) = course_carried(&state.catalog, tenant_id, course_id).await
    {
        return refusal;
    }
    let Some(rule_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(RoutingRuleId::new)
    else {
        return floor_entropy_unavailable();
    };
    let new_rule = NewRoutingRule {
        rule_id,
        tenant_id,
        store_id,
        station_id,
        menu_item_id,
        course_id,
        sort: request.sort,
    };
    store_routing_rule(&state, &context, &new_rule).await
}

/// Stores a rule, records it, and answers its id.
async fn store_routing_rule<F, A, C, Cat>(
    state: &FloorState<F, A, C, Cat>,
    context: &AdminContext,
    rule: &NewRoutingRule,
) -> Response
where
    F: RoutingRuleStore,
    C: ClockSource,
{
    match RoutingRuleStore::create(&state.floor, rule).await {
        Ok(()) => {
            record_routing_create(state, context, rule).await;
            (
                StatusCode::CREATED,
                Json(serde_json::json!({ "id": rule.rule_id.to_string() })),
            )
                .into_response()
        }
        Err(error) => floor_error_response(&error),
    }
}

/// What a rule matches on: exactly one of an item or a course, each a ULID when sent.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it is the 400 the caller returns"
)]
fn routing_target(
    request: &CreateRoutingRuleRequest,
) -> Result<(Option<MenuItemId>, Option<CourseId>), Response> {
    let Ok(menu_item_id) = parse_optional_ulid(request.menu_item_id.as_deref(), MenuItemId::new)
    else {
        return Err(ulid_refusal(&["menu_item_id"]));
    };
    let Ok(course_id) = parse_optional_ulid(request.course_id.as_deref(), CourseId::new) else {
        return Err(ulid_refusal(&["course_id"]));
    };
    // A rule must match exactly one of an item or a course — the same rule the §10 validator enforces
    // at publish, surfaced here so the console cannot store a rule that matches nothing or both.
    if menu_item_id.is_some() == course_id.is_some() {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "a routing rule must match exactly one of menu_item_id or course_id",
            &[
                ("menu_item_id", "MUTUALLY_EXCLUSIVE"),
                ("course_id", "MUTUALLY_EXCLUSIVE"),
            ],
        ));
    }
    Ok((menu_item_id, course_id))
}

/// The course a rule matches on must be one the tenant actually carries, and still carries
/// ([ADR-0130](../../../docs/adr/0130-a-course-is-something-the-catalog-names.md) decision 6).
///
/// Refused at the write rather than at publish, the way the menu graph's cycle is (`would_cycle`)
/// and for the same reason: a rule that can never match is a rule an operator will never find out
/// about. It publishes, the store honours it, and it routes nothing — for ever, silently, because
/// no line can carry that id either.
///
/// **Archived counts as gone.** The compiler does not publish an archived course, so a rule
/// naming one would be exactly as dead as a rule naming an id that never existed; the two are one
/// answer here so they cannot drift into different behaviour later.
async fn course_carried<Cat>(
    catalog: &Cat,
    tenant_id: TenantId,
    course_id: CourseId,
) -> Result<(), Response>
where
    Cat: CatalogStore,
{
    let carried = match catalog.list_courses(tenant_id).await {
        Ok(rows) => rows.into_iter().any(|row| {
            row.record.course_id == course_id && row.record.status != EntityStatus::Archived
        }),
        Err(error) => return Err(catalog_error_response(&error)),
    };
    if !carried {
        return Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "a routing rule must match a course this tenant carries",
            &[("course_id", "NOT_FOUND")],
        ));
    }
    Ok(())
}

/// Records a created rule with what it routes: the station, the item or the course, and its sort.
async fn record_routing_create<F, A, C, Cat>(
    state: &FloorState<F, A, C, Cat>,
    context: &AdminContext,
    rule: &NewRoutingRule,
) where
    C: ClockSource,
{
    let after = serde_json::json!({
        "id": rule.rule_id.to_string(),
        "station_id": rule.station_id.to_string(),
        "menu_item_id": rule.menu_item_id.map(|id| id.to_string()),
        "course_id": rule.course_id.map(|id| id.to_string()),
        "sort": rule.sort,
    });
    audit_action(
        &state.audit,
        &state.clock,
        context,
        Some(rule.tenant_id),
        "kitchen.routing.create",
        "station_routing_rule",
        &rule.rule_id.to_string(),
        None,
        Some(after),
    )
    .await;
}

pub(super) async fn admin_remove_routing<F, A, C, Cat>(
    State(state): State<FloorState<F, A, C, Cat>>,
    headers: HeaderMap,
    Path(rule_id): Path<String>,
    Query(query): Query<FloorTenantQuery>,
) -> Response
where
    F: RoutingRuleStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Cat: CatalogStore + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageFloor,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, rule_id) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("rule_id", &rule_id)]) {
            Ok([tenant_id, rule_id]) => (TenantId::new(tenant_id), RoutingRuleId::new(rule_id)),
            Err(refusal) => return refusal,
        };
    match RoutingRuleStore::remove(&state.floor, tenant_id, rule_id).await {
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "kitchen.routing.remove",
                "station_routing_rule",
                &rule_id.to_string(),
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("routing rule"),
        Err(error) => floor_error_response(&error),
    }
}
