// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /admin/people/publish`: a tenant's people and roles compiled into a store's `permissions` node.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{parse_ulid_fields, service_unavailable};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};
use serde::Deserialize;

use super::requests::people_error_response;
use crate::audit::AuditRecorder;
use crate::auth::admin::{AdminContext, AdminStore, system_actor};
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::http::{PublishedConfig, publish_config_nodes, publish_config_nodes_with};
use crate::people::{AssignmentStore, Employee, EmployeeStore, RoleTemplate, RoleTemplateStore};
use crate::people_compiler::compile_permissions;
use crate::people_republish::Republished;

// --- People publish (`/admin/people/publish`, ADR-0070 slice 5) ---------------------------------

/// The collaborators the people-publish route needs: the people store (the employee, role, and
/// assignment seams plus the trusted PIN-hash read), the config-tree store the compiled node is written
/// onto, and the admin/clock/audit every write carries. Separate from [`PeopleState`] because
/// publishing also needs the config-tree seam, exactly as catalog publish ([`CatalogPublishState`]) is
/// separate from catalog authoring.
#[derive(Clone)]
struct PeoplePublishState<P, Cfg, A, C> {
    people: P,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// Builds the people-publish sub-router ([ADR-0070](../../../../../docs/adr/0070-people-and-access.md) slice 5).
///
/// One route: compile a store's people + roles + assignments into the edge-shaped `permissions`
/// document and write it onto the store's `permissions` config node, versioned through the config tree
/// like every other publish. Behind [`ConsolePermission::PublishConfig`], the same gate as catalog and
/// config publish.
pub fn people_publish_router<P, Cfg, A, C>(
    people: P,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/people/publish",
            post(admin_publish_permissions::<P, Cfg, A, C>),
        )
        .with_state(PeoplePublishState {
            people,
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// A super-admin selects the (tenant, store) whose `permissions` node to compile and publish.
#[derive(Debug, Clone, Deserialize)]
struct PublishPermissionsRequest {
    tenant_id: String,
    store_id: String,
}

/// Compiles one store's roster into its `permissions` node, with the staff count for the trail.
///
/// Unlike the other twelve batchable nodes this one is genuinely **per store**: the assignments and
/// therefore the document differ at every member, so a batch calls this once per store rather than
/// compiling once and fanning out ([ADR-0122](../../../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md) §4).
/// The instruction is still store-agnostic — "recompile each of these shops' own rosters" — which
/// is what makes it batchable at all.
pub(in crate::http) async fn permissions_nodes<P>(
    people: &P,
    tenant_id: TenantId,
    store_id: StoreId,
) -> Result<(Vec<(String, serde_json::Value)>, usize), Response>
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore,
{
    // Load the domain the compiler needs. `list` is on two of P's traits, so the calls are
    // fully-qualified to say which seam each one is.
    let assignments = match AssignmentStore::list_for_store(people, tenant_id, store_id).await {
        Ok(assignments) => assignments,
        Err(error) => return Err(people_error_response(&error)),
    };
    // As with the floor, the compiler takes the authoring records; the version each was read at is
    // a writer's concern (ADR-0094) and has no place in a published permissions document.
    let employees: Vec<Employee> = match EmployeeStore::list(people, tenant_id).await {
        Ok(employees) => employees
            .into_iter()
            .map(|employee| employee.record)
            .collect(),
        Err(error) => return Err(people_error_response(&error)),
    };
    let roles: Vec<RoleTemplate> = match RoleTemplateStore::list(people, tenant_id).await {
        Ok(roles) => roles.into_iter().map(|role| role.record).collect(),
        Err(error) => return Err(people_error_response(&error)),
    };
    // The stored PIN hash for each assigned employee, read only here (the trusted publish path) and
    // never returned over the API. Deduplicated: a store assigns a person at most once, but reading
    // by a set keeps it robust.
    let mut pins = std::collections::BTreeMap::new();
    for assignment in &assignments {
        if pins.contains_key(&assignment.employee_id.to_string()) {
            continue;
        }
        let hash = match EmployeeStore::pin_phc(people, tenant_id, assignment.employee_id).await {
            Ok(hash) => hash,
            Err(error) => return Err(people_error_response(&error)),
        };
        pins.insert(assignment.employee_id.to_string(), hash);
    }

    let document = compile_permissions(store_id, &employees, &roles, &assignments, &pins);
    let staff_count = document.staff.len();
    let Ok(document_value) = serde_json::to_value(&document) else {
        tracing::error!("could not serialise a compiled permissions document");
        return Err(service_unavailable("people"));
    };
    Ok((
        vec![("permissions".to_owned(), document_value)],
        staff_count,
    ))
}

/// Compiles a store's people into its `permissions` config node and versions it through the config
/// tree — the same load→compile→write→version shape as [`admin_publish_menu`], onto the `permissions`
/// key instead of `menu`/`layout`. The PIN hash rides in the node (the edge verifies against it,
/// ADR-0030); the audit records only the config version and staff count, never a name or PIN.
async fn admin_publish_permissions<P, Cfg, A, C>(
    State(state): State<PeoplePublishState<P, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishPermissionsRequest>,
) -> Response
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Clone + Send + Sync + 'static,
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

    let publisher = PermissionsPublisher {
        people: &state.people,
        config_trees: &state.config_trees,
        clock: &state.clock,
        audit: &state.audit,
    };
    match publisher.publish(&context, tenant_id, store_id, None).await {
        Ok(id) => (
            StatusCode::OK,
            Json(PublishedConfig {
                config_version_id: id.to_string(),
            }),
        )
            .into_response(),
        Err(refusal) => refusal,
    }
}

/// The collaborators a `permissions` publish needs, borrowed from whichever route's state holds
/// them: the people publish's, and the people routes' own, whose removals and archives publish at
/// once ([ADR-0158](../../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
/// decision 7).
///
/// One publish for both is the point. A store is sent the node its people compile to, written and
/// audited the same way, whether someone pressed publish or offboarded a person; two copies of the
/// steps could drift until the two sent different nodes.
pub(in crate::http) struct PermissionsPublisher<'a, P, Cfg, C> {
    pub(in crate::http) people: &'a P,
    pub(in crate::http) config_trees: &'a Cfg,
    pub(in crate::http) clock: &'a C,
    pub(in crate::http) audit: &'a Arc<dyn AuditRecorder>,
}

impl<P, Cfg, C> PermissionsPublisher<'_, P, Cfg, C>
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore,
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    /// Compiles one store's roster, writes it onto the store's `permissions` node and versions it
    /// through the config tree — the same load→compile→write→version shape as
    /// [`admin_publish_menu`], onto the `permissions` key. The PIN hash rides in the node (the edge
    /// verifies against it, ADR-0030).
    ///
    /// The trail records the config version and how many staff the node carries — never a name or
    /// a PIN (ADR-0070) — and, for a publish a write caused, the id that write was about, under its
    /// own key: `assignment_id`, `employee_id`, `role_template_id` or `store_group_id`.
    async fn publish(
        &self,
        context: &AdminContext,
        tenant_id: TenantId,
        store_id: StoreId,
        cause: Option<(&'static str, &str)>,
    ) -> Result<ConfigVersionId, Response> {
        // Set the `permissions` key on the store's Store layer (index 2 in
        // Tenant→Brand→Store→Device) and re-publish that layer, preserving any other Store-level
        // keys (`menu`, `layout`, …).
        let (nodes, staff_count) = permissions_nodes(self.people, tenant_id, store_id).await?;
        let id = publish_config_nodes(
            self.config_trees,
            self.clock,
            tenant_id,
            store_id,
            ConfigLevel::Store,
            nodes,
        )
        .await?;
        self.record(context, tenant_id, store_id, id, staff_count, cause)
            .await;
        Ok(id)
    }

    /// Publishes a store's people again, as [`Self::publish`] does, where the node the store holds
    /// is not what they compile to now: what the people republisher does for a store a migration
    /// queued after changing what roles grant ([`crate::people_republish`]).
    ///
    /// The node is compared inside the write's retry, on the tree each attempt reads. A store whose
    /// configuration holds no `permissions` roster, a node with a `staff` list, on whichever layer
    /// a publish or a rollback left it, is one whose people were never published, and is given
    /// none: the edge reads a node without `staff` as the switch alone. A store whose Store layer
    /// already holds what its people compile to is given no new version. Either abandons the write,
    /// as a refusal abandons it, and records nothing. Anything else, a rolled-back store whose
    /// roster sits on its Tenant layer included, is written onto the Store layer, as **Publish**
    /// writes it.
    #[expect(
        clippy::result_large_err,
        reason = "the composing closure's Err is an axum Response by design — the shape every node \
                  publish's closure carries — and here it only abandons a write with nothing to \
                  publish; what the tree held is the outcome"
    )]
    async fn republish(
        &self,
        context: &AdminContext,
        tenant_id: TenantId,
        store_id: StoreId,
        reason: &str,
    ) -> Result<Republished, Response> {
        /// What a store's tree held, as an attempt of the write read it.
        enum Held {
            /// No `permissions` roster in what the store runs: its people were never published.
            Nothing,
            /// On its Store layer, the node its people compile to now.
            Same,
            /// A roster, and not that node on the Store layer: a stale one, or one a rollback
            /// moved onto the Tenant layer.
            Stale,
        }
        const NODE: &str = pos_proto::people::PublishedPermissions::NODE;
        let (nodes, staff_count) = permissions_nodes(self.people, tenant_id, store_id).await?;
        let mut held = None;
        let written = publish_config_nodes_with(
            self.config_trees,
            self.clock,
            tenant_id,
            store_id,
            ConfigLevel::Store,
            |tree| {
                // Read from the four layers merged, the document the store runs, because a
                // rollback moves the whole restored version onto the Tenant layer. A `permissions`
                // node there is a roster only when it has a `staff` key, the edge's own test:
                // without one it is the switch alone, and the store's people were never published.
                let rostered = tree.is_some_and(|tree| {
                    tree.effective()
                        .get(NODE)
                        .and_then(|node| node.get("staff"))
                        .is_some()
                });
                let current = tree
                    .and_then(|tree| tree.layer(ConfigLevel::Store).get(NODE))
                    .is_some_and(|node| {
                        nodes
                            .iter()
                            .any(|(key, compiled)| key == NODE && compiled == node)
                    });
                let found = match (rostered, current) {
                    (false, _) => Held::Nothing,
                    (true, true) => Held::Same,
                    (true, false) => Held::Stale,
                };
                let write = matches!(found, Held::Stale);
                held = Some(found);
                if write {
                    Ok(nodes.clone())
                } else {
                    Err(StatusCode::NO_CONTENT.into_response())
                }
            },
        )
        .await;
        match (written, held) {
            (Ok(id), _) => {
                self.record(
                    context,
                    tenant_id,
                    store_id,
                    id,
                    staff_count,
                    Some(("reason", reason)),
                )
                .await;
                Ok(Republished::Published(id))
            }
            (Err(_), Some(Held::Nothing)) => Ok(Republished::NeverPublished),
            (Err(_), Some(Held::Same)) => Ok(Republished::Unchanged),
            (Err(refusal), Some(Held::Stale) | None) => Err(refusal),
        }
    }

    /// Records a `permissions` publish: the config version and how many staff the node carries —
    /// never a name, a code or a PIN (ADR-0070, ADR-0158 decision 9) — and what caused it, under
    /// its own key.
    async fn record(
        &self,
        context: &AdminContext,
        tenant_id: TenantId,
        store_id: StoreId,
        id: ConfigVersionId,
        staff_count: usize,
        cause: Option<(&'static str, &str)>,
    ) {
        let mut after = serde_json::json!({
            "config_version_id": id.to_string(),
            "staff_count": staff_count,
        });
        if let (Some((key, value)), Some(fields)) = (cause, after.as_object_mut()) {
            fields.insert(key.to_owned(), serde_json::Value::from(value));
        }
        audit_action(
            self.audit,
            self.clock,
            context,
            Some(tenant_id),
            "permissions.publish",
            "store",
            &store_id.to_string(),
            None,
            Some(after),
        )
        .await;
    }

    /// Publishes to every store a write reached, in turn, and answers with how each went: the `200`
    /// a write that publishes at once returns, `{"stores": [...]}`, the shape a settings write
    /// answers with.
    pub(super) async fn publish_each(
        &self,
        context: &AdminContext,
        tenant_id: TenantId,
        stores: &BTreeSet<StoreId>,
        cause: (&'static str, &str),
    ) -> Response {
        let results = self.publish_all(context, tenant_id, stores, cause).await;
        (
            StatusCode::OK,
            Json(serde_json::json!({ "stores": results })),
        )
            .into_response()
    }

    /// Publishes to every store a write reached, in turn, and reports how each went, for a write
    /// whose answer carries the report beside its own fields.
    ///
    /// One store failing does not stop the others: each answers for itself, and a failed one is
    /// published again with `POST /admin/people/publish`. The write that caused this has already
    /// been saved by then, so a failure here is reported rather than refused — taking the write
    /// back would be a second change nobody asked for.
    pub(in crate::http) async fn publish_all(
        &self,
        context: &AdminContext,
        tenant_id: TenantId,
        stores: &BTreeSet<StoreId>,
        cause: (&'static str, &str),
    ) -> Vec<PermissionsPublishResult> {
        let mut results = Vec::with_capacity(stores.len());
        for store_id in stores {
            let published = self
                .publish(context, tenant_id, *store_id, Some(cause))
                .await;
            results.push(match published {
                Ok(id) => PermissionsPublishResult {
                    store_id: store_id.to_string(),
                    outcome: "PERMISSIONS_PUBLISH_APPLIED",
                    config_version_id: Some(id.to_string()),
                },
                Err(_refused) => {
                    tracing::warn!(%store_id, "a store's permissions node could not be published");
                    PermissionsPublishResult {
                        store_id: store_id.to_string(),
                        outcome: "PERMISSIONS_PUBLISH_FAILED",
                        config_version_id: None,
                    }
                }
            });
        }
        results
    }
}

/// Publishes one queued store's people again for the people republisher
/// ([`crate::people_republish`]): through the publish **Publish** on People goes through, recorded
/// under the system actor ([`crate::auth::admin::system_actor`]) with the queue's `reason`.
///
/// # Errors
///
/// The refusal the publish would have answered with, when the store's people or its tree could
/// not be read or the tree refused the write. The store stays queued.
pub(crate) async fn republish_people<P, Cfg, C>(
    people: &P,
    config_trees: &Cfg,
    clock: &C,
    audit: &Arc<dyn AuditRecorder>,
    tenant_id: TenantId,
    store_id: StoreId,
    reason: &str,
) -> Result<Republished, Response>
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore,
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    PermissionsPublisher {
        people,
        config_trees,
        clock,
        audit,
    }
    .republish(&system_actor(), tenant_id, store_id, reason)
    .await
}

/// How one store's `permissions` publish went, as a write that publishes at once reports it.
#[derive(Debug, Clone, serde::Serialize)]
pub(in crate::http) struct PermissionsPublishResult {
    store_id: String,
    /// `PERMISSIONS_PUBLISH_APPLIED` or `PERMISSIONS_PUBLISH_FAILED`.
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_version_id: Option<String>,
}
