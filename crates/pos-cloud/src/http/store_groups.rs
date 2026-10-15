// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Store groups (`/admin/store-groups`): the cohorts a tenant delivers configuration to, and the
//! batch publish that fans a node out over a group's stores (ADR-0122).
//!
//! The state, the staff a membership change moves, the request bodies, the router and the error
//! helper are here.

use std::collections::BTreeSet;
use std::sync::Arc;

use axum::Router;
use axum::response::Response;
use axum::routing::{get, post};
use groups::{
    admin_create_store_group, admin_list_store_groups, admin_set_store_group_members,
    admin_update_store_group,
};
use history::{admin_list_store_group_batches, admin_read_store_group_batch};
use pos_cloud_api::response::api_error;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use publish::admin_publish_to_store_group;
use serde::{Deserialize, Serialize};

use crate::audit::AuditRecorder;
use crate::auth::admin::{AdminContext, AdminStore};
use crate::config_tree::ConfigTreeStore;
use crate::http::people::{PermissionsPublishResult, PermissionsPublisher};
use crate::people::{AssignmentStore, AssignmentStoreError, EmployeeStore, RoleTemplateStore};
use crate::registry::{EntityStatus, RegistryStore};
use crate::store_groups::{StoreGroupId, StoreGroupStore, StoreGroupStoreError};

mod batch;
mod batch_node;
mod data_nodes;
mod groups;
mod history;
mod publish;
mod settings_nodes;

pub use batch_node::{BatchNode, BatchNodePlan};
pub use data_nodes::batch_nodes;
// What the people, settings and release-schedule routes use.
pub(super) use batch_node::BoxFuture;
pub(super) use publish::{publishable_membership, unknown_batch_node};
// The handlers the console's OpenAPI document names.
pub(crate) use groups::{
    __path_admin_create_store_group, __path_admin_list_store_groups,
    __path_admin_set_store_group_members, __path_admin_update_store_group,
};
pub(crate) use history::{
    __path_admin_list_store_group_batches, __path_admin_read_store_group_batch,
};
pub(crate) use publish::__path_admin_publish_to_store_group;

// --- Store groups (`/admin/store-groups`, ADR-0122) ---------------------------------------------

/// The most stores one group may hold ([ADR-0122](../../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md) §8).
///
/// A batch publish runs synchronously inside its request and fans out over the membership, so the
/// cap is what keeps that request finite: at roughly 10–30 ms per store publish, 200 is a handful of
/// seconds. `docs/capacity-and-reliability.md` models tenants up to 1,000 stores, and such a tenant
/// uses several groups — which is how an operator of that size already thinks about their estate
/// (by city, by format, by pilot cohort). The alternative is a background job queue, which ADR-0122
/// rejects *for now* and names the trigger for: a real tenant needing one cohort above this number.
const MAX_STORE_GROUP_MEMBERS: usize = 200;

/// The collaborators the store-group routes need.
///
/// The registry is here for one reason and it is worth stating: a membership write checks every id
/// against the tenant's stores. Without that a typo'd ULID sits in the cohort permanently, and every
/// batch reports it `skipped` forever — a cohort quietly one store short of what the operator
/// believes it is.
#[derive(Clone)]
struct StoreGroupState<Grp, Reg, Cfg, A, C> {
    groups: Grp,
    registry: Reg,
    /// Where a batch publish writes: the same seam every single-store publish already uses.
    config_trees: Cfg,
    /// The node kinds a batch may publish, built once in `main.rs` — see [`batch_nodes`].
    nodes: Arc<Vec<Arc<dyn BatchNode>>>,
    /// Who works through each group, told when a store joins or leaves one (ADR-0158).
    staff: Arc<dyn GroupStaff>,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// What a change to a group's membership asks of the people side
/// ([ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
/// decisions 3 and 7): an assignment that names the group reaches each store it holds, so a store
/// that joins or leaves the group gains or loses those people, and is told at once.
///
/// A trait object for the reason [`GroupMembership`] is one, from the other side: the store-group
/// handlers would otherwise all carry the people store's type for the one route that reads it.
trait GroupStaff: Send + Sync {
    /// Whether any assignment names the group.
    fn names_group(
        &self,
        tenant_id: TenantId,
        group_id: StoreGroupId,
    ) -> BoxFuture<'_, Result<bool, AssignmentStoreError>>;

    /// Publishes `permissions` to each store, in turn, with the group as the cause the trail
    /// records, and reports how each went.
    fn publish_to<'a>(
        &'a self,
        context: &'a AdminContext,
        tenant_id: TenantId,
        group_id: StoreGroupId,
        stores: &'a BTreeSet<StoreId>,
    ) -> BoxFuture<'a, Vec<PermissionsPublishResult>>;
}

/// [`GroupStaff`] over the people store, with what a `permissions` publish writes through.
struct PeopleOfGroups<P, Cfg, C> {
    people: P,
    config_trees: Cfg,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

impl<P, Cfg, C> GroupStaff for PeopleOfGroups<P, Cfg, C>
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Send + Sync,
    Cfg: ConfigTreeStore + Send + Sync,
    C: ClockSource + Send + Sync,
{
    fn names_group(
        &self,
        tenant_id: TenantId,
        group_id: StoreGroupId,
    ) -> BoxFuture<'_, Result<bool, AssignmentStoreError>> {
        Box::pin(async move {
            AssignmentStore::list_for_group(&self.people, tenant_id, group_id)
                .await
                .map(|named| !named.is_empty())
        })
    }

    fn publish_to<'a>(
        &'a self,
        context: &'a AdminContext,
        tenant_id: TenantId,
        group_id: StoreGroupId,
        stores: &'a BTreeSet<StoreId>,
    ) -> BoxFuture<'a, Vec<PermissionsPublishResult>> {
        Box::pin(async move {
            let cause = group_id.to_string();
            PermissionsPublisher {
                people: &self.people,
                config_trees: &self.config_trees,
                clock: &self.clock,
                audit: &self.audit,
            }
            .publish_all(context, tenant_id, stores, ("store_group_id", &cause))
            .await
        })
    }
}

/// A store group on the wire, with its membership.
///
/// Members ride along with the list rather than needing a read per group: the console's first
/// question about a cohort is how big it is, and a screen that had to fetch each one would show a
/// list of names with the counts arriving afterwards.
#[derive(Debug, Clone, Serialize)]
struct StoreGroupView {
    group_id: String,
    tenant_id: String,
    name: String,
    status: EntityStatus,
    store_ids: Vec<String>,
    etag: String,
}

/// A `POST /admin/store-groups` body. The id is server-minted.
#[derive(Debug, Clone, Deserialize)]
struct CreateStoreGroupRequest {
    tenant_id: String,
    name: String,
}

/// A `PATCH /admin/store-groups/{group_id}` body: rename and/or set the status.
#[derive(Debug, Clone, Deserialize)]
struct UpdateStoreGroupRequest {
    tenant_id: String,
    name: String,
    status: String,
}

/// A `PUT /admin/store-groups/{group_id}/members` body: the whole membership, not a delta.
///
/// Wholesale rather than add/remove because the operator's mental model is a *set* — "these are the
/// airport shops" — and a delta API makes two admins' concurrent edits merge into a cohort neither
/// of them chose. Under `If-Match` (ADR-0095 shape C) the second one is refused instead.
#[derive(Debug, Clone, Deserialize)]
struct SetStoreGroupMembersRequest {
    tenant_id: String,
    store_ids: Vec<String>,
}

/// Builds the store-group sub-router ([ADR-0122](../../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md)).
///
/// CRUD over the cohorts a tenant publishes to. Reads are behind [`ConsolePermission::Read`]; the
/// writes are behind [`ConsolePermission::ManageStores`], the same permission that creates and
/// renames a store — a group is a way of *organising* stores, not a way of changing what they run,
/// and publishing to one is a separate act behind `console.config.publish`.
///
/// Ids are server-minted: a batch names its group forever in `config_batches`, so a client that
/// could choose one could collide with a cohort's history.
///
/// `people` is the roster a membership change reaches: a store that joins or leaves a group some
/// assignment names is published its `permissions` node at once
/// ([ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)).
pub fn store_group_router<Grp, Reg, Cfg, P, A, C>(
    groups: Grp,
    registry: Reg,
    config_trees: Cfg,
    nodes: Vec<Arc<dyn BatchNode>>,
    people: P,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
    Reg: RegistryStore + Clone + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let staff = Arc::new(PeopleOfGroups {
        people,
        config_trees: config_trees.clone(),
        clock: clock.clone(),
        audit: Arc::clone(&audit),
    });
    Router::new()
        .route(
            "/admin/store-groups",
            get(admin_list_store_groups::<Grp, Reg, Cfg, A, C>)
                .post(admin_create_store_group::<Grp, Reg, Cfg, A, C>),
        )
        .route(
            "/admin/store-groups/{group_id}",
            axum::routing::patch(admin_update_store_group::<Grp, Reg, Cfg, A, C>),
        )
        .route(
            "/admin/store-groups/{group_id}/members",
            axum::routing::put(admin_set_store_group_members::<Grp, Reg, Cfg, A, C>),
        )
        .route(
            "/admin/store-groups/{group_id}/publish",
            post(admin_publish_to_store_group::<Grp, Reg, Cfg, A, C>),
        )
        .route(
            "/admin/store-groups/{group_id}/batches",
            get(admin_list_store_group_batches::<Grp, Reg, Cfg, A, C>),
        )
        .route(
            "/admin/store-groups/{group_id}/batches/{batch_id}",
            get(admin_read_store_group_batch::<Grp, Reg, Cfg, A, C>),
        )
        .with_state(StoreGroupState {
            groups,
            registry,
            config_trees,
            nodes: Arc::new(nodes),
            staff,
            admin,
            clock,
            audit,
        })
}

/// The `503` a store-group persistence failure becomes.
pub(in crate::http) fn store_group_error_response(error: &StoreGroupStoreError) -> Response {
    tracing::error!(%error, "a store-group operation failed");
    api_error(
        ErrorStatus::Unavailable,
        "the store-group service is unavailable",
    )
}
