// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The checks a role update and an assignment run before they write: what a role update would
//! change, and whether a person, role or store can be assigned at all.

use std::collections::BTreeSet;

use axum::response::Response;
use pos_cloud_api::response::api_error_with_details;
use pos_proto::ErrorStatus;
use pos_proto::ids::{StoreId, TenantId};

use super::PeopleState;
use super::requests::{UpdateRoleRequest, people_error_response};
use super::roles::granted_with_approval;
use crate::http::{not_found, registry_error_response, store_group_error_response};
use crate::people::{
    AssignmentScope, AssignmentStore, EmployeeId, EmployeeStore, RoleTemplateId, RoleTemplateStore,
    RoleTemplateUpdate,
};
use crate::registry::{EntityStatus, RegistryStore};

/// The update `PATCH /admin/roles/{role_id}` writes, and the stores it publishes to at once, from
/// the request and the role as stored, read once before the write and `404` if there is none.
///
/// The stored role answers two things. A request that leaves `permissions_with_approval` out keeps
/// what the role grants with approval ([`granted_with_approval`]). And an update that archives the
/// role reaches every store an assignment granting it reaches, through a group or the tenant as
/// well — read before the write, for the reason [`admin_update_employee`] gives — while one that
/// leaves it active, or finds it archived already, reaches none (`None`).
pub(super) async fn planned_role_update<P, R, Cfg, A, C>(
    state: &PeopleState<P, R, Cfg, A, C>,
    tenant_id: TenantId,
    role_template_id: RoleTemplateId,
    request: &UpdateRoleRequest,
    status: EntityStatus,
) -> Result<(RoleTemplateUpdate, Option<BTreeSet<StoreId>>), Response>
where
    P: RoleTemplateStore + AssignmentStore,
    R: RegistryStore,
{
    let stored = match RoleTemplateStore::get(&state.people, tenant_id, role_template_id).await {
        Ok(Some(stored)) => stored.record,
        Ok(None) => return Err(not_found("role")),
        Err(error) => return Err(people_error_response(&error)),
    };
    let permissions_with_approval = granted_with_approval(
        &request.permissions,
        request.permissions_with_approval.as_deref(),
        &stored.permissions_with_approval,
    )?;
    let reached = if status == EntityStatus::Archived && stored.status != EntityStatus::Archived {
        let assignments =
            AssignmentStore::list_for_role(&state.people, tenant_id, role_template_id)
                .await
                .map_err(|error| people_error_response(&error))?;
        let scopes = assignments.iter().map(|assignment| assignment.scope);
        Some(state.stores_reached(tenant_id, scopes).await?)
    } else {
        None
    };
    let update = RoleTemplateUpdate {
        role_template_id,
        tenant_id,
        name: request.name.clone(),
        permissions: request.permissions.clone(),
        permissions_with_approval,
        discount_ceiling_minor: request.discount_ceiling_minor,
        status,
    };
    Ok((update, reached))
}

/// Refuses an assignment to an employee, store, group or role the tenant does not have (`404`) or
/// has archived (`409`), each named by the request field that carried its id — the route's half of
/// the referential check the schema leaves to it (ADR-0070, ADR-0158 decision 7). A group is looked
/// up among the tenant's own, so another tenant's is `404` like one that never existed; an
/// assignment to every store names nothing beyond the person and the role.
pub(super) async fn refuse_unassignable<P, R, Cfg, A, C>(
    state: &PeopleState<P, R, Cfg, A, C>,
    tenant_id: TenantId,
    employee_id: EmployeeId,
    scope: AssignmentScope,
    role_template_id: RoleTemplateId,
) -> Result<(), Response>
where
    P: EmployeeStore + RoleTemplateStore,
    R: RegistryStore,
{
    match EmployeeStore::get(&state.people, tenant_id, employee_id).await {
        Ok(Some(employee)) if employee.record.status == EntityStatus::Archived => {
            return Err(unassignable_archived(
                "employee_id",
                "the employee is archived; restore them before assigning them to a store",
            ));
        }
        Ok(Some(_)) => {}
        Ok(None) => return Err(unassignable_unknown("employee_id", "employee")),
        Err(error) => return Err(people_error_response(&error)),
    }
    match scope {
        AssignmentScope::Store(store_id) => match state.registry.list_stores(tenant_id).await {
            Ok(stores) => match stores
                .into_iter()
                .find(|store| store.record.store_id == store_id)
            {
                Some(store) if store.record.status == EntityStatus::Archived => {
                    return Err(unassignable_archived(
                        "store_id",
                        "the store is archived; restore it before assigning anyone to it",
                    ));
                }
                Some(_) => {}
                None => return Err(unassignable_unknown("store_id", "store")),
            },
            Err(error) => return Err(registry_error_response(&error)),
        },
        AssignmentScope::StoreGroup(group_id) => {
            match state.groups.group(tenant_id, group_id).await {
                Ok(Some(group)) if group.status == EntityStatus::Archived => {
                    return Err(unassignable_archived(
                        "store_group_id",
                        "the store group is archived; restore it before assigning anyone to it",
                    ));
                }
                Ok(Some(_)) => {}
                Ok(None) => return Err(unassignable_unknown("store_group_id", "store group")),
                Err(error) => return Err(store_group_error_response(&error)),
            }
        }
        AssignmentScope::Tenant => {}
    }
    match RoleTemplateStore::get(&state.people, tenant_id, role_template_id).await {
        Ok(Some(role)) if role.record.status == EntityStatus::Archived => {
            Err(unassignable_archived(
                "role_template_id",
                "the role is archived; restore it before assigning it to anyone",
            ))
        }
        Ok(Some(_)) => Ok(()),
        Ok(None) => Err(unassignable_unknown("role_template_id", "role")),
        Err(error) => Err(people_error_response(&error)),
    }
}

/// The `404` an assignment earns for an id its tenant does not have, naming the request field that
/// carried it.
fn unassignable_unknown(field: &str, entity: &str) -> Response {
    api_error_with_details(
        ErrorStatus::NotFound,
        format!("no such {entity}"),
        &[(field, "NOT_FOUND")],
    )
}

/// The `409` an assignment earns for an id its tenant has archived (ADR-0158 decision 7), naming
/// the request field that carried it. A conflict with the referent's state rather than a bad field,
/// which is the line `FAILED_PRECONDITION` draws: the request is right, and restoring the referent
/// makes it succeed.
fn unassignable_archived(field: &str, message: &str) -> Response {
    api_error_with_details(
        ErrorStatus::FailedPrecondition,
        message,
        &[(field, "ARCHIVED")],
    )
}
