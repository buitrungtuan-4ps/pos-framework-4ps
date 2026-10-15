// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! People & access: the employees, role templates and assignments a tenant authors under
//! `/admin/employees`, `/admin/roles` and `/admin/assignments`, the capability catalogue and its
//! publish, the people publish, and a store's permissions readiness
//! ([ADR-0070](../../../../docs/adr/0070-people-and-access.md)).
//!
//! The state, the rules it reads from a store's settings, and the router are here.

use std::collections::BTreeSet;
use std::sync::Arc;

use assignments::{admin_create_assignment, admin_list_assignments, admin_remove_assignment};
use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use employees::{
    admin_create_employee, admin_get_employee, admin_list_employees, admin_set_employee_pin,
    admin_update_employee,
};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::session::PublishedSession;
use pos_proto::settings::SettingScope;
use roles::{
    admin_create_role, admin_get_role, admin_list_permissions, admin_list_roles, admin_update_role,
};

use crate::audit::AuditRecorder;
use crate::auth::admin::{AdminContext, AdminStore};
use crate::config_tree::ConfigTreeStore;
use crate::http::{BoxFuture, registry_error_response, store_group_error_response, with_etag};
use crate::people::{AssignmentScope, AssignmentStore, EmployeeStore, RoleTemplateStore};
use crate::registry::{EntityStatus, RegistryStore};
use crate::settings::{SettingsStore, SettingsStoreError};
use crate::store_groups::{StoreGroupId, StoreGroupStore, StoreGroupStoreError};
use crate::version::Version;

mod assignment_scope;
mod assignments;
mod capabilities;
mod checks;
mod employees;
mod publish;
mod readiness;
mod requests;
mod roles;

pub use capabilities::{capabilities_router, config_capabilities_router};
pub use publish::people_publish_router;
pub use readiness::permissions_readiness_router;

// What the batch fan-out, the store groups and the people republisher use, and the handler the
// console's OpenAPI document names.
pub(super) use capabilities::capability_flag_nodes;
pub(crate) use publish::republish_people;
pub(super) use publish::{PermissionsPublishResult, PermissionsPublisher, permissions_nodes};
pub(crate) use readiness::__path_admin_permissions_readiness;
pub(super) use requests::people_error_response;

// --- People & access (`/admin/employees|roles|assignments`, ADR-0070) ---------------------------

/// The collaborators the people routes need, stated independently of [`CloudApp`]: the people store
/// (one type that is the employee, role-template, and assignment seam at once — the binary's
/// `PostgresPeople` implements all three), plus the admin and clock every session guard uses, and the
/// audit recorder every write emits to. Like [`RegistryState`], it carries its own state and is merged
/// into the main router.
///
/// The registry, the store groups and the config trees are here for
/// [ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) decisions 3
/// and 7: an assignment is refused for a store or group the tenant does not have or has archived,
/// and a removal, an archive or a new assignment to a group or every store publishes the
/// `permissions` node to every store it affects in the same request.
#[derive(Clone)]
struct PeopleState<P, R, Cfg, A, C> {
    people: P,
    registry: R,
    /// The tenant's store groups, through which a group assignment reaches its stores (ADR-0122).
    groups: Arc<dyn GroupMembership>,
    /// The fewest digits the tenant lets a PIN have, which the PIN route applies (ADR-0160).
    pin_minimum: Arc<dyn PinMinimum>,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// What the people routes read of a tenant's store groups
/// ([ADR-0122](../../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md)): whether a group
/// exists, its status and the stores it holds.
///
/// A trait object rather than a sixth type parameter on [`PeopleState`], for the reason
/// [`BatchNode`] gives: every people handler would carry the store-group store's type for the few
/// that read it.
trait GroupMembership: Send + Sync {
    /// The group's status and members, or `None` when the tenant has no such group — one of
    /// another tenant included.
    fn group(
        &self,
        tenant_id: TenantId,
        group_id: StoreGroupId,
    ) -> BoxFuture<'_, Result<Option<GroupMembers>, StoreGroupStoreError>>;
}

/// One group as [`GroupMembership`] reads it.
struct GroupMembers {
    status: EntityStatus,
    store_ids: Vec<StoreId>,
}

impl<G> GroupMembership for G
where
    G: StoreGroupStore + Send + Sync,
{
    fn group(
        &self,
        tenant_id: TenantId,
        group_id: StoreGroupId,
    ) -> BoxFuture<'_, Result<Option<GroupMembers>, StoreGroupStoreError>> {
        Box::pin(async move {
            let Some(group) = self
                .list_groups(tenant_id)
                .await?
                .into_iter()
                .find(|group| group.record.group_id == group_id)
            else {
                return Ok(None);
            };
            let store_ids = self.list_members(tenant_id, group_id).await?;
            Ok(Some(GroupMembers {
                status: group.record.status,
                store_ids,
            }))
        })
    }
}

/// What the PIN route reads of a tenant's settings: the fewest digits a PIN may have,
/// `session.pin_min_length`
/// ([ADR-0160](../../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 2). A trait object rather than another type parameter on [`PeopleState`], for the reason
/// [`GroupMembership`] gives: one handler reads it.
trait PinMinimum: Send + Sync {
    /// The fewest digits a PIN set now may have at `tenant_id`: the value the tenant wrote for every
    /// store, read through [`PublishedSession::pin_min_length`] so its bounds and its default, four,
    /// are the register's.
    fn fewest_digits(&self, tenant_id: TenantId) -> BoxFuture<'_, Result<u32, SettingsStoreError>>;
}

/// The register key the PIN route reads.
const PIN_MIN_LENGTH_KEY: &str = "session.pin_min_length";

impl<S> PinMinimum for S
where
    S: SettingsStore + Send + Sync,
{
    fn fewest_digits(&self, tenant_id: TenantId) -> BoxFuture<'_, Result<u32, SettingsStoreError>> {
        Box::pin(async move {
            let tenant = tenant_id.as_ulid().to_string();
            let written = self.list(tenant_id).await?.into_iter().find(|value| {
                value.setting_key == PIN_MIN_LENGTH_KEY
                    && value.scope.known() == SettingScope::Tenant
                    && value.scope_id == tenant
            });
            let session = PublishedSession {
                pin_min_length: written.and_then(|value| value.value.as_i64()),
                ..PublishedSession::default()
            };
            Ok(session.pin_min_length())
        })
    }
}

impl<P, R, Cfg, A, C> PeopleState<P, R, Cfg, A, C> {
    /// The stores a set of assignment scopes reaches now, each once and in order: the store a
    /// one-store scope names, the open stores a group holds, and every open store of the tenant
    /// (ADR-0158 decision 3).
    ///
    /// A one-store scope reaches its store whatever the store's status, as it always has. A wider
    /// one reaches only the open stores, the ones a group batch publishes to as well: an archived
    /// store is not published to merely because it is in a group or is the tenant's. An archived
    /// group still reaches the stores it holds — archiving a delivery cohort does not take anybody's
    /// access away — and a group that is gone reaches none.
    async fn stores_reached(
        &self,
        tenant_id: TenantId,
        scopes: impl IntoIterator<Item = AssignmentScope>,
    ) -> Result<BTreeSet<StoreId>, Response>
    where
        R: RegistryStore,
    {
        let mut reached = BTreeSet::new();
        let mut groups = BTreeSet::new();
        let mut tenant_wide = false;
        for scope in scopes {
            match scope {
                AssignmentScope::Store(store_id) => {
                    reached.insert(store_id);
                }
                AssignmentScope::StoreGroup(group_id) => {
                    groups.insert(group_id);
                }
                AssignmentScope::Tenant => tenant_wide = true,
            }
        }
        if groups.is_empty() && !tenant_wide {
            return Ok(reached);
        }
        let open: BTreeSet<StoreId> = match self.registry.list_stores(tenant_id).await {
            Ok(stores) => stores
                .into_iter()
                .filter(|store| store.record.status == EntityStatus::Active)
                .map(|store| store.record.store_id)
                .collect(),
            Err(error) => return Err(registry_error_response(&error)),
        };
        if tenant_wide {
            reached.extend(open);
            return Ok(reached);
        }
        for group_id in groups {
            match self.groups.group(tenant_id, group_id).await {
                Ok(Some(group)) => {
                    let members = group.store_ids.into_iter();
                    reached.extend(members.filter(|member| open.contains(member)));
                }
                Ok(None) => {}
                Err(error) => return Err(store_group_error_response(&error)),
            }
        }
        Ok(reached)
    }

    /// The publish `POST /admin/people/publish` makes, over these routes' own collaborators.
    fn publisher(&self) -> PermissionsPublisher<'_, P, Cfg, C> {
        PermissionsPublisher {
            people: &self.people,
            config_trees: &self.config_trees,
            clock: &self.clock,
            audit: &self.audit,
        }
    }

    /// What an employee or role update answers once it is saved, always with the record's new
    /// version: `204` when it published nothing, or — when it archived the record, and `reached`
    /// holds the stores read before the write — the report of publishing to each of them at once
    /// (ADR-0158 decision 7).
    async fn answer_update(
        &self,
        context: &AdminContext,
        tenant_id: TenantId,
        version: &Version,
        reached: Option<BTreeSet<StoreId>>,
        (cause, cause_id): (&'static str, String),
    ) -> Response
    where
        P: EmployeeStore + RoleTemplateStore + AssignmentStore,
        Cfg: ConfigTreeStore,
        C: ClockSource,
    {
        let Some(reached) = reached else {
            return with_etag(StatusCode::NO_CONTENT.into_response(), version);
        };
        let report = self
            .publisher()
            .publish_each(context, tenant_id, &reached, (cause, &cause_id))
            .await;
        with_etag(report, version)
    }
}

/// Builds the people & access sub-router ([ADR-0070](../../../../docs/adr/0070-people-and-access.md)).
///
/// The staff records — the employee list and read-one, and the assignment list, whose rows carry the
/// person's name and code — are behind [`ConsolePermission::ReadPeople`] (Owner/Admin,
/// [ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md) decision 9).
/// The permission catalogue and the role templates name nobody, so they stay behind
/// [`ConsolePermission::Read`] (every console role). Every write is behind
/// [`ConsolePermission::ManagePeople`] (Owner/Admin) and emits an audit entry that records the
/// employee **id, code, status, and role — never the name, and never the PIN or its hash** (ADR-0070).
/// `GET /admin/audit` shows that code only to a role holding `ReadPeople`, which is the roster's
/// gate. The tenant is named the admin-is-global way: a `?tenant_id=` query on reads, the request
/// body on writes.
///
/// What the console authors is what the store enforces
/// ([ADR-0158](../../../../docs/adr/0158-the-till-enforces-each-persons-own-permissions.md)
/// decisions 3 and 7). An assignment reaches one store, a store group or every store of the tenant,
/// and is refused for an employee, store, group or role the tenant does not have (`404`) or has
/// archived (`409`). Removing an assignment, archiving a person, archiving a role and creating an
/// assignment to a group or every store publish the `permissions` node to every store the change
/// reaches before they answer, and answer with each store's outcome; every other write publishes as
/// before, when someone presses publish.
///
/// Setting a PIN reads the tenant's `settings` for the fewest digits a PIN may have (ADR-0160).
pub fn people_router<P, R, G, St, Cfg, A, C>(
    people: P,
    registry: R,
    groups: G,
    settings: St,
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    P: EmployeeStore + RoleTemplateStore + AssignmentStore + Clone + Send + Sync + 'static,
    R: RegistryStore + Clone + Send + Sync + 'static,
    G: StoreGroupStore + Send + Sync + 'static,
    St: SettingsStore + Send + Sync + 'static,
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/people/permissions",
            get(admin_list_permissions::<P, R, Cfg, A, C>),
        )
        .route(
            "/admin/employees",
            get(admin_list_employees::<P, R, Cfg, A, C>)
                .post(admin_create_employee::<P, R, Cfg, A, C>),
        )
        .route(
            "/admin/employees/{employee_id}",
            get(admin_get_employee::<P, R, Cfg, A, C>)
                .patch(admin_update_employee::<P, R, Cfg, A, C>),
        )
        .route(
            "/admin/employees/{employee_id}/pin",
            axum::routing::put(admin_set_employee_pin::<P, R, Cfg, A, C>),
        )
        .route(
            "/admin/roles",
            get(admin_list_roles::<P, R, Cfg, A, C>).post(admin_create_role::<P, R, Cfg, A, C>),
        )
        .route(
            "/admin/roles/{role_id}",
            get(admin_get_role::<P, R, Cfg, A, C>).patch(admin_update_role::<P, R, Cfg, A, C>),
        )
        .route(
            "/admin/assignments",
            get(admin_list_assignments::<P, R, Cfg, A, C>)
                .post(admin_create_assignment::<P, R, Cfg, A, C>),
        )
        .route(
            "/admin/assignments/{assignment_id}",
            delete(admin_remove_assignment::<P, R, Cfg, A, C>),
        )
        .with_state(PeopleState {
            people,
            registry,
            groups: Arc::new(groups),
            pin_minimum: Arc::new(settings),
            config_trees,
            admin,
            clock,
            audit,
        })
}
