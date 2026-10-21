// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Creating a config release: its target stores and the timezone each resolves its moment in.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{
    api_error, api_error_with_details, parse_ulid_fields, service_unavailable,
};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use super::config_releases::{
    CreateReleaseRequest, ReleaseState, ReleaseView, moment_from_request, release_error_response,
};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::ConfigTreeStore;
use crate::http::store_groups::publishable_membership;
use crate::http::{Json, read_store_node_as_run, registry_error_response};
use crate::registry::{EntityStatus, RegistryStore};
use crate::releases::{
    NewRelease, Release, ReleaseStatus, ReleaseStore, ResolveRefusal, TargetStore,
};
use crate::scheduling::ScheduledPublishStore;
use crate::store_groups::{StoreGroupId, StoreGroupStore};

#[utoipa::path(
    post,
    path = "/admin/config-releases",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.config.publish"))),
    responses(
        (status = 201, description = "The release exists as a draft. It carries no nodes and no \
                                      pairs yet; `/schedule` adds both"),
        (status = 400, description = "An id is not a ULID, the name is empty, or the moment is \
                                      half-given (a date without a time) or doubly-given (a \
                                      wall-clock time and an instant)", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The release store is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "config releases",
)]
/// A super-admin names a release, aims it, and times it. Nothing is scheduled yet.
///
/// A draft, deliberately: a release across nine nodes and forty stores is assembled over minutes,
/// and half-assembled must not fire (ADR-0125 §4). The nodes and the concrete store list arrive at
/// `/schedule`, which is also when the snapshot is taken.
pub(super) async fn admin_create_release<Rel, Sch, Grp, Reg, Cfg, A, C>(
    State(state): State<ReleaseState<Rel, Sch, Grp, Reg, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<CreateReleaseRequest>,
) -> Response
where
    Rel: ReleaseStore + Clone + Send + Sync + 'static,
    Sch: ScheduledPublishStore + Clone + Send + Sync + 'static,
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
    Reg: RegistryStore + Clone + Send + Sync + 'static,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &request.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let name = request.name.trim().to_owned();
    if name.is_empty() {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            "a release needs a name an operator will recognise in a month",
            &[("name", "REQUIRED")],
        );
    }
    if let Some(group_id) = &request.target_group_id
        && let Err(refusal) = parse_ulid_fields([("target_group_id", group_id)])
    {
        return refusal;
    }
    let moment = match moment_from_request(&request) {
        Ok(moment) => moment,
        Err(refusal) => return refusal,
    };

    let now_ms = state.clock.now().as_milliseconds_since_epoch();
    let Some(id) = mint_ulid(now_ms) else {
        return service_unavailable("releases");
    };
    let release = NewRelease {
        id: id.to_string(),
        tenant_id,
        name: name.clone(),
        target_group_id: request.target_group_id.clone(),
        moment: moment.clone(),
        created_by: context.admin.id.clone(),
    };
    if let Err(error) = state.releases.create(&release).await {
        return release_error_response(&error);
    }
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "release.created",
        "release",
        &release.id,
        None,
        Some(serde_json::json!({ "name": name })),
    )
    .await;
    (
        StatusCode::CREATED,
        Json(ReleaseView::of(&Release {
            id: release.id,
            tenant_id,
            name,
            status: ReleaseStatus::Draft,
            target_group_id: request.target_group_id,
            moment,
            created_by: context.admin.id,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
        })),
    )
        .into_response()
}

/// Resolves a release's target to concrete, publishable store ids.
///
/// Explicit ids are checked against the tenant's registry rather than trusted, for the reason
/// ADR-0122 gives for membership: an id that names nothing sits in the release forever and reports
/// as un-applied at every read — a release quietly one store short of what the operator believes.
pub(in crate::http) async fn release_target_stores<Grp, Reg>(
    groups: &Grp,
    registry: &Reg,
    tenant_id: TenantId,
    target_group_id: Option<&str>,
    explicit: Option<&[String]>,
) -> Result<Vec<StoreId>, Response>
where
    Grp: StoreGroupStore,
    Reg: RegistryStore,
{
    if let Some(ids) = explicit {
        if ids.is_empty() {
            return Err(api_error(
                ErrorStatus::Unprocessable,
                "this release names no stores, so it would reach nobody",
            ));
        }
        let known = match registry.list_stores(tenant_id).await {
            Ok(rows) => rows
                .into_iter()
                .map(|row| (row.record.store_id, row.record.status))
                .collect::<std::collections::BTreeMap<_, _>>(),
            Err(error) => return Err(registry_error_response(&error)),
        };
        let mut stores = Vec::with_capacity(ids.len());
        for id in ids {
            let store_id = match parse_ulid_fields([("store_ids", id)]) {
                Ok([parsed]) => StoreId::new(parsed),
                Err(refusal) => return Err(refusal),
            };
            match known.get(&store_id) {
                None => {
                    return Err(api_error(
                        ErrorStatus::Unprocessable,
                        format!("store {store_id} is not in this tenant's registry"),
                    ));
                }
                Some(EntityStatus::Archived) => {
                    return Err(api_error(
                        ErrorStatus::Unprocessable,
                        format!("store {store_id} is archived; restore it before releasing to it"),
                    ));
                }
                Some(_) => stores.push(store_id),
            }
        }
        return Ok(stores);
    }
    let Some(group_id) = target_group_id else {
        return Err(api_error(
            ErrorStatus::Unprocessable,
            "this release has no target: name a store group when creating it, or store_ids here",
        ));
    };
    let group_id = match parse_ulid_fields([("target_group_id", group_id)]) {
        Ok([parsed]) => StoreGroupId::new(parsed),
        Err(refusal) => return Err(refusal),
    };
    publishable_membership(groups, tenant_id, group_id).await
}

/// Reads each target store's `locale.timezone` as the store runs it ([`read_store_node_as_run`]),
/// for the wall-clock conversion.
///
/// A store with no `locale` node comes back with `None`, which is not an error here: an instant
/// release needs no timezone at all, and only [`resolve_for_stores`](crate::releases::resolve_for_stores) knows whether this release is
/// one. The refusal, when it comes, names the store.
pub(in crate::http) async fn release_target_timezones<Cfg>(
    config_trees: &Cfg,
    tenant_id: TenantId,
    stores: &[StoreId],
) -> Result<Vec<TargetStore>, Response>
where
    Cfg: ConfigTreeStore,
{
    let mut targets = Vec::with_capacity(stores.len());
    for store_id in stores {
        let locale = read_store_node_as_run(config_trees, tenant_id, *store_id, "locale").await?;
        targets.push(TargetStore {
            store_id: *store_id,
            timezone: locale
                .get("timezone")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        });
    }
    Ok(targets)
}

/// Turns every refusal into one `422` naming every store it could not time.
///
/// Every store, not the first: a forty-store release that reported one gap per attempt would take
/// forty round trips to become schedulable, and an operator fixing timezones wants the list.
pub(in crate::http) fn unresolvable_moment(refusals: &[(StoreId, ResolveRefusal)]) -> Response {
    let named: Vec<String> = refusals
        .iter()
        .map(|(store_id, refusal)| format!("{store_id}: {refusal}"))
        .collect();
    api_error(
        ErrorStatus::Unprocessable,
        format!(
            "this release cannot be timed at {} of its stores — {}",
            refusals.len(),
            named.join("; ")
        ),
    )
}
