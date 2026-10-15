// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Where an assignment applies: its scope, its ids, and the refusals and audit record of each.

use axum::response::Response;
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::{ErrorStatus, WireEnum};

use super::requests::CreateAssignmentRequest;
use crate::people::{
    AssignmentId, AssignmentScope, AssignmentScopeKind, EmployeeId, RoleTemplateId,
};
use crate::store_groups::StoreGroupId;
use pos_cloud_api::refusal::{accepted_tokens, enum_refusal};

/// The `409` an assignment earns where the person already holds one: a person holds one per store,
/// one per group and one tenant-wide (ADR-0158 decision 3). It names the field that names the place,
/// `store_id`, `store_group_id` or `scope_kind` for every store.
///
/// A conflict rather than an outage: before the store said which, this was the people service's
/// `503`, which sent the operator to retry what can never succeed. What resolves it is the
/// assignment that is there, changed or removed.
pub(super) fn already_assigned(scope: AssignmentScope) -> Response {
    let field = match scope {
        AssignmentScope::Store(_) => "store_id",
        AssignmentScope::StoreGroup(_) => "store_group_id",
        AssignmentScope::Tenant => "scope_kind",
    };
    api_error_with_details(
        ErrorStatus::AlreadyExists,
        "the employee is already assigned there: change or remove that assignment instead",
        &[(field, "ALREADY_EXISTS")],
    )
}

/// The scope a new assignment's fields name, with the id that names it as sent, before it is
/// parsed.
#[derive(Debug, Clone, Copy)]
pub(super) enum ScopeTarget<'a> {
    /// One store, by `store_id`.
    Store(&'a str),
    /// A store group, by `store_group_id`.
    StoreGroup(&'a str),
    /// Every store of the tenant, by no id.
    Tenant,
}

/// A new assignment's ids, parsed in one go so a refusal names each that is not a ULID: the tenant,
/// the person, the scope with the id `target` sent, and the role.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
pub(super) fn assignment_ids(
    request: &CreateAssignmentRequest,
    target: ScopeTarget<'_>,
) -> Result<(TenantId, EmployeeId, AssignmentScope, RoleTemplateId), Response> {
    let tenant = ("tenant_id", request.tenant_id.as_str());
    let employee = ("employee_id", request.employee_id.as_str());
    let role = ("role_template_id", request.role_template_id.as_str());
    let named = match target {
        ScopeTarget::Store(id) => ("store_id", id),
        ScopeTarget::StoreGroup(id) => ("store_group_id", id),
        ScopeTarget::Tenant => {
            return parse_ulid_fields([tenant, employee, role]).map(|[tenant, employee, role]| {
                (
                    TenantId::new(tenant),
                    EmployeeId::new(employee),
                    AssignmentScope::Tenant,
                    RoleTemplateId::new(role),
                )
            });
        }
    };
    parse_ulid_fields([tenant, employee, named, role]).map(|[tenant, employee, id, role]| {
        let scope = if let ScopeTarget::StoreGroup(_) = target {
            AssignmentScope::StoreGroup(StoreGroupId::new(id))
        } else {
            AssignmentScope::Store(StoreId::new(id))
        };
        (
            TenantId::new(tenant),
            EmployeeId::new(employee),
            scope,
            RoleTemplateId::new(role),
        )
    })
}

/// Which scope a new assignment names, as the request carries it (ADR-0158 decision 3).
///
/// `scope_kind` says which. Absent, or `ASSIGNMENT_SCOPE_UNSPECIFIED`, the id sent says it:
/// `store_id` alone is one store, as every assignment was, and `store_group_id` alone is a group.
/// Each scope takes its own id and no other — one store its `store_id`, a group its
/// `store_group_id`, every store neither — and anything else is a `400` naming each field at fault:
/// `REQUIRED` for the id the scope needs, `MUTUALLY_EXCLUSIVE` for one it does not take.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns"
)]
pub(super) fn requested_scope(
    request: &CreateAssignmentRequest,
) -> Result<ScopeTarget<'_>, Response> {
    let kind = match request.scope_kind.as_deref() {
        None => AssignmentScopeKind::Unspecified,
        Some(token) => match AssignmentScopeKind::from_wire(token) {
            Some(kind) => kind,
            None => {
                return Err(enum_refusal(
                    "scope_kind",
                    accepted_tokens::<AssignmentScopeKind>(),
                ));
            }
        },
    };
    let store = request.store_id.as_deref();
    let group = request.store_group_id.as_deref();
    match (kind, store, group) {
        (AssignmentScopeKind::Store | AssignmentScopeKind::Unspecified, Some(store), None) => {
            Ok(ScopeTarget::Store(store))
        }
        (AssignmentScopeKind::StoreGroup | AssignmentScopeKind::Unspecified, None, Some(group)) => {
            Ok(ScopeTarget::StoreGroup(group))
        }
        (AssignmentScopeKind::Tenant, None, None) => Ok(ScopeTarget::Tenant),
        // With no kind to say which, both ids are the mistake together, and neither alone.
        (AssignmentScopeKind::Unspecified, Some(_), Some(_)) => Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "name one of store_id or store_group_id, not both",
            &[
                ("store_id", "MUTUALLY_EXCLUSIVE"),
                ("store_group_id", "MUTUALLY_EXCLUSIVE"),
            ],
        )),
        (AssignmentScopeKind::Unspecified, None, None) => Err(api_error_with_details(
            ErrorStatus::InvalidArgument,
            "store_id is required: name one store, a store_group_id, or scope_kind \
             ASSIGNMENT_SCOPE_TENANT for every store",
            &[("store_id", "REQUIRED")],
        )),
        (kind, store, group) => Err(scope_refusal(kind, store.is_some(), group.is_some())),
    }
}

/// The `400` an assignment earns when `scope_kind` names a scope its ids do not fit, naming each id
/// at fault: the one the scope needs and did not get, and the one it does not take.
fn scope_refusal(kind: AssignmentScopeKind, sent_store: bool, sent_group: bool) -> Response {
    let (takes_store, takes_group, message) = match kind {
        AssignmentScopeKind::StoreGroup => (
            false,
            true,
            "an assignment to a store group names its store_group_id, and no store_id",
        ),
        AssignmentScopeKind::Tenant => (
            false,
            false,
            "an assignment to every store names neither a store_id nor a store_group_id",
        ),
        AssignmentScopeKind::Store | AssignmentScopeKind::Unspecified => (
            true,
            false,
            "an assignment to one store names its store_id, and no store_group_id",
        ),
    };
    let mut details = Vec::with_capacity(2);
    for (field, sent, takes) in [
        ("store_id", sent_store, takes_store),
        ("store_group_id", sent_group, takes_group),
    ] {
        if sent != takes {
            details.push((
                field,
                if sent {
                    "MUTUALLY_EXCLUSIVE"
                } else {
                    "REQUIRED"
                },
            ));
        }
    }
    api_error_with_details(ErrorStatus::InvalidArgument, message, &details)
}

/// What the trail records of an assignment: its ids and its scope, the `store_id` or
/// `store_group_id` it names beside its `scope_kind` — never the name or code a read resolves
/// beside them (ADR-0070).
pub(super) fn assignment_audit(
    assignment_id: AssignmentId,
    employee_id: EmployeeId,
    scope: AssignmentScope,
    role_template_id: RoleTemplateId,
) -> serde_json::Value {
    let mut entry = serde_json::Map::new();
    entry.insert("id".to_owned(), assignment_id.to_string().into());
    entry.insert("employee_id".to_owned(), employee_id.to_string().into());
    entry.insert("scope_kind".to_owned(), scope.kind().as_wire().into());
    if let Some(store_id) = scope.store_id() {
        entry.insert("store_id".to_owned(), store_id.to_string().into());
    }
    if let Some(store_group_id) = scope.store_group_id() {
        entry.insert(
            "store_group_id".to_owned(),
            store_group_id.to_string().into(),
        );
    }
    entry.insert(
        "role_template_id".to_owned(),
        role_template_id.to_string().into(),
    );
    serde_json::Value::Object(entry)
}
