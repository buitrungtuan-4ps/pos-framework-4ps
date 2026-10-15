// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/assignments`: list, create and remove who holds which role where.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::{ErrorStatus, WireEnum};

use super::PeopleState;
use super::assignment_scope::{
    already_assigned, assignment_audit, assignment_ids, requested_scope,
};
use super::checks::refuse_unassignable;
use super::requests::{
    AssignmentListQuery, AssignmentListing, CreateAssignmentRequest, PeopleTenantQuery,
    people_entropy_unavailable, people_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::http::{enum_refusal, not_found};
use crate::people::{
    Assignment, AssignmentId, AssignmentScope, AssignmentScopeKind, AssignmentStore,
    AssignmentStoreError, EmployeeId, EmployeeStore, NewAssignment, RoleTemplateStore,
};
use crate::registry::RegistryStore;
use crate::store_groups::StoreGroupId;

/// A super-admin lists assignments, by exactly one of four selectors: everyone who works at a store
/// (`?store_id=` — every assignment that reaches it: its own, its groups' and the tenant's), all a
/// person holds (`?employee_id=`), those naming a store group (`?store_group_id=`), or those that
/// reach every store (`?scope_kind=ASSIGNMENT_SCOPE_TENANT`). Each row says its scope. Behind
/// [`ConsolePermission::ReadPeople`], the roster's gate: each row names the person it grants
/// ([`Assignment`]).
pub(super) async fn admin_list_assignments<P, R, Cfg, A, C>(
    State(state): State<PeopleState<P, R, Cfg, A, C>>,
    headers: HeaderMap,
    Query(query): Query<AssignmentListQuery>,
) -> Response
where
    P: AssignmentStore + Clone + Send + Sync + 'static,
    R: Clone + Send + Sync + 'static,
    Cfg: Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) = require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ReadPeople,
    )
    .await
    {
        return denied;
    }
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let selectors = [
        (
            "store_id",
            AssignmentListing::Store,
            query.store_id.as_deref(),
        ),
        (
            "employee_id",
            AssignmentListing::Employee,
            query.employee_id.as_deref(),
        ),
        (
            "store_group_id",
            AssignmentListing::Group,
            query.store_group_id.as_deref(),
        ),
        (
            "scope_kind",
            AssignmentListing::TenantWide,
            query.scope_kind.as_deref(),
        ),
    ];
    let mut named = selectors
        .iter()
        .filter_map(|(field, listing, value)| value.map(|value| (*field, *listing, value)));
    let (Some((field, listing, value)), None) = (named.next(), named.next()) else {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name exactly one of store_id, employee_id, store_group_id or scope_kind",
            &selectors.map(|(field, _, _)| (field, "MUTUALLY_EXCLUSIVE")),
        );
    };
    let result = match listing {
        AssignmentListing::Store => match parse_ulid_fields([(field, value)]) {
            Ok([id]) => {
                state
                    .people
                    .list_for_store(tenant_id, StoreId::new(id))
                    .await
            }
            Err(refusal) => return refusal,
        },
        AssignmentListing::Employee => match parse_ulid_fields([(field, value)]) {
            Ok([id]) => {
                let employee_id = EmployeeId::new(id);
                state.people.list_for_employee(tenant_id, employee_id).await
            }
            Err(refusal) => return refusal,
        },
        AssignmentListing::Group => match parse_ulid_fields([(field, value)]) {
            Ok([id]) => {
                let store_group_id = StoreGroupId::new(id);
                state.people.list_for_group(tenant_id, store_group_id).await
            }
            Err(refusal) => return refusal,
        },
        // Only the tenant's rows are listed by their kind: a store's and a group's are listed by
        // the id they name.
        AssignmentListing::TenantWide => {
            if AssignmentScopeKind::from_wire(value) != Some(AssignmentScopeKind::Tenant) {
                return enum_refusal(field, [AssignmentScopeKind::Tenant.as_wire()]);
            }
            state.people.list_tenant_wide(tenant_id).await
        }
    };
    match result {
        Ok(assignments) => (StatusCode::OK, Json::<Vec<Assignment>>(assignments)).into_response(),
        Err(error) => people_error_response(&error),
    }
}

/// A super-admin assigns an employee with a role, at one store, a store group or every store of the
/// tenant ([ADR-0158](../../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
/// decision 3; [`requested_scope`] reads which). Audited `assignment.create` with the ids and the
/// scope ([`assignment_audit`]).
///
/// Each id is checked before anything is written, because the schema declares no foreign keys
/// under row-level security and leaves the check to this route (ADR-0070). An employee, store,
/// group or role the tenant does not have is `404` — another tenant's group included — and one it
/// has archived is `409` (decision 7): a grant to a retired person, store or group would publish
/// nobody, and one with a retired role would grant nothing, while the console showed it as a grant.
/// Each refusal names the field that carried the id. A person who already holds an assignment
/// there is `409` too ([`already_assigned`]), and nothing is written, audited or published.
///
/// An assignment to one store publishes as before, when someone presses publish, and answers `201`
/// with its id. One to a group or to every store publishes the `permissions` node at once to every
/// store it reaches — the open stores the group holds, or every open store of the tenant, read
/// before the write — and answers `201` with its id and each store's outcome (`stores`).
pub(super) async fn admin_create_assignment<P, R, Cfg, A, C>(
    State(state): State<PeopleState<P, R, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateAssignmentRequest>,
) -> Response
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Clone + Send + Sync + 'static,
    R: RegistryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManagePeople,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let target = match requested_scope(&request) {
        Ok(target) => target,
        Err(refusal) => return refusal,
    };
    let (tenant_id, employee_id, scope, role_template_id) = match assignment_ids(&request, target) {
        Ok(ids) => ids,
        Err(refusal) => return refusal,
    };
    let checked =
        refuse_unassignable(&state, tenant_id, employee_id, scope, role_template_id).await;
    if let Err(refusal) = checked {
        return refusal;
    }
    // A wider assignment changes who works at every store it reaches, and each is told at once.
    // They are read before the write, for the reason `admin_update_employee` gives.
    let reached = if let AssignmentScope::Store(_) = scope {
        None
    } else {
        match state.stores_reached(tenant_id, [scope]).await {
            Ok(stores) => Some(stores),
            Err(refusal) => return refusal,
        }
    };
    let Some(assignment_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(AssignmentId::new)
    else {
        return people_entropy_unavailable();
    };
    let new_assignment = NewAssignment {
        assignment_id,
        tenant_id,
        employee_id,
        scope,
        role_template_id,
    };
    match state.people.assign(&new_assignment).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "assignment.create",
                "assignment",
                &assignment_id.to_string(),
                None,
                Some(assignment_audit(
                    assignment_id,
                    employee_id,
                    scope,
                    role_template_id,
                )),
            )
            .await;
            let created = match reached {
                None => serde_json::json!({ "id": assignment_id.to_string() }),
                Some(reached) => {
                    let stores = state
                        .publisher()
                        .publish_all(
                            &context,
                            tenant_id,
                            &reached,
                            ("assignment_id", &assignment_id.to_string()),
                        )
                        .await;
                    serde_json::json!({ "id": assignment_id.to_string(), "stores": stores })
                }
            };
            (StatusCode::CREATED, Json(created)).into_response()
        }
        Err(AssignmentStoreError::AlreadyAssigned(_)) => already_assigned(scope),
        Err(error) => people_error_response(&error),
    }
}

/// A super-admin removes an assignment — offboarding a person from where it reached. Audited
/// `assignment.remove` when a row was actually removed, with the ids and the scope it bound; a no-op
/// removal records nothing.
///
/// The removal publishes the `permissions` node at once to every store the assignment reached — its
/// store, the open stores its group holds, or every open store of the tenant — so the person cannot
/// sign in there a moment longer
/// ([ADR-0158](../../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
/// decision 7), and answers `200` with each store's outcome.
pub(super) async fn admin_remove_assignment<P, R, Cfg, A, C>(
    State(state): State<PeopleState<P, R, Cfg, A, C>>,
    headers: HeaderMap,
    Path(assignment_id): Path<String>,
    Query(query): Query<PeopleTenantQuery>,
) -> Response
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Clone + Send + Sync + 'static,
    R: RegistryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManagePeople,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let (tenant_id, assignment_id) = match parse_ulid_fields([
        ("tenant_id", &query.tenant_id),
        ("assignment_id", &assignment_id),
    ]) {
        Ok([tenant_id, assignment_id]) => {
            (TenantId::new(tenant_id), AssignmentId::new(assignment_id))
        }
        Err(refusal) => return refusal,
    };
    // Read before the removal: the row names the stores that must be told, and it is gone after.
    let assignment = match AssignmentStore::get(&state.people, tenant_id, assignment_id).await {
        Ok(Some(assignment)) => assignment,
        Ok(None) => return not_found("assignment"),
        Err(error) => return people_error_response(&error),
    };
    let reached = match state.stores_reached(tenant_id, [assignment.scope]).await {
        Ok(stores) => stores,
        Err(refusal) => return refusal,
    };
    match state.people.remove(tenant_id, assignment_id).await {
        Ok(true) => {
            // The ids the grant bound, so the trail says who was offboarded from where and under
            // which role — never the name or code the read resolved beside them (ADR-0070).
            let before = assignment_audit(
                assignment_id,
                assignment.employee_id,
                assignment.scope,
                assignment.role_template_id,
            );
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "assignment.remove",
                "assignment",
                &assignment_id.to_string(),
                Some(before),
                None,
            )
            .await;
            state
                .publisher()
                .publish_each(
                    &context,
                    tenant_id,
                    &reached,
                    ("assignment_id", &assignment_id.to_string()),
                )
                .await
        }
        Ok(false) => not_found("assignment"),
        Err(error) => people_error_response(&error),
    }
}
