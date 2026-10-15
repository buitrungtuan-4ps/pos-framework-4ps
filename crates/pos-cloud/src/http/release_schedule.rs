// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The steps [`admin_schedule_release`](super::config_publish::release_actions::admin_schedule_release) takes, in the order it takes
//! them: each refuses with the response to send, and only the last writes
//! ([ADR-0125](../../../../docs/adr/0125-a-release-is-one-decision-many-writes.md)).

use std::sync::Arc;

use axum::response::Response;

use pos_proto::ErrorStatus;
use pos_proto::ids::{StoreId, TenantId};

use super::setting_refusal::unconfirmed_response;
use super::{
    AdminContext, AdminStore, BatchNode, ClockSource, RegistryStore, ReleaseNodeRequest,
    ReleaseState, ScheduleReleaseRequest, StoreGroupStore, api_error, audit_action,
    config_store_error_response, mint_ulid, not_found, prerequisites_for, read_store_node_as_run,
    release_error_response, release_report_response, release_target_stores,
    release_target_timezones, scheduled_error_response, service_unavailable, unknown_batch_node,
    unresolvable_moment,
};
use crate::config_tree::{ConfigTreeState, ConfigTreeStore};
use crate::releases::{
    Release, ReleaseMoment, ReleaseStatus, ReleaseStore, ResolveRefusal, ResolvedStore,
    TargetStore, resolve_for_stores,
};
use crate::scheduling::{
    NewScheduledPublish, ScheduledPublishStore, check_confirmations as check_one,
};

/// A node the release carries, with the arguments its plan takes.
type CarriedNode<'r> = (Arc<dyn BatchNode>, &'r serde_json::Value);

/// The release to schedule and its moment, if it is a draft that has one and the request carries
/// nodes.
pub(super) async fn draft_to_schedule<Rel: ReleaseStore>(
    releases: &Rel,
    tenant_id: TenantId,
    release_id: &str,
    request: &ScheduleReleaseRequest,
) -> Result<(Release, ReleaseMoment), Response> {
    let release = match releases.fetch_one(tenant_id, release_id).await {
        Ok(Some(release)) => release,
        Ok(None) => return Err(not_found("release")),
        Err(error) => return Err(release_error_response(&error)),
    };
    if release.status != ReleaseStatus::Draft {
        return Err(api_error(
            ErrorStatus::Unprocessable,
            "this release has already been scheduled; cancel it and make another",
        ));
    }
    let Some(moment) = release.moment.clone() else {
        return Err(api_error(
            ErrorStatus::Unprocessable,
            "this release has no moment; give it a wall-clock time or an instant before scheduling",
        ));
    };
    if request.nodes.is_empty() {
        return Err(api_error(
            ErrorStatus::Unprocessable,
            "this release carries no nodes, so scheduling it would publish nothing",
        ));
    }
    Ok((release, moment))
}

/// Each node the request names, with its arguments, or the refusal for one that is not a node.
///
/// Settle the instruction before anything is read per store: an unknown node is the request's own
/// mistake and would be the same answer at every shop.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the refusal the caller returns"
)]
pub(super) fn settle_nodes<'r>(
    nodes: &[Arc<dyn BatchNode>],
    asked: &'r [ReleaseNodeRequest],
) -> Result<Vec<CarriedNode<'r>>, Response> {
    let mut carried: Vec<CarriedNode<'_>> = Vec::with_capacity(asked.len());
    for asked in asked {
        let Some(node) = nodes.iter().find(|candidate| candidate.key() == asked.node) else {
            return Err(unknown_batch_node(nodes, &asked.node));
        };
        carried.push((Arc::clone(node), &asked.arguments));
    }
    Ok(carried)
}

/// The moment read in each store's timezone, or the refusal naming every store it cannot be.
///
/// One wall-clock time is one instant per timezone: a fleet spanning Ho Chi Minh City and Tokyo
/// has a two-hour spread, and this is where it is written down (ADR-0125 §2).
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the refusal the caller returns"
)]
pub(super) fn resolve_instants(
    moment: &ReleaseMoment,
    targets: &[TargetStore],
) -> Result<Vec<ResolvedStore>, Response> {
    let resolved = resolve_for_stores(moment, targets);
    let refusals: Vec<(StoreId, ResolveRefusal)> = resolved
        .iter()
        .filter_map(|store| match &store.outcome {
            Err(refusal) => Some((store.store_id, refusal.clone())),
            Ok(_) => None,
        })
        .collect();
    if !refusals.is_empty() {
        return Err(unresolvable_moment(&refusals));
    }
    Ok(resolved)
}

/// Refuses when a store lacks a node a carried node needs and the release does not carry it.
///
/// The schedule-time half of ADR-0125 §6. A prerequisite the release carries itself is satisfied
/// by the release — the activator applies them in dependency order — so only the ones it does not
/// carry are read from the store.
pub(super) async fn check_prerequisites<Cfg: ConfigTreeStore>(
    config_trees: &Cfg,
    tenant_id: TenantId,
    stores: &[StoreId],
    carried: &[CarriedNode<'_>],
    carried_keys: &[&str],
) -> Result<(), Response> {
    for (node, _) in carried {
        for needed in prerequisites_for(node.key()) {
            if carried_keys.contains(needed) {
                continue;
            }
            for store_id in stores {
                match read_store_node_as_run(config_trees, tenant_id, *store_id, needed).await {
                    Ok(serde_json::Value::Null) => {
                        return Err(api_error(
                            ErrorStatus::Unprocessable,
                            format!(
                                "store {store_id} has no `{needed}` node, which `{}` needs; publish \
                                 one to it or carry `{needed}` in this release",
                                node.key()
                            ),
                        ));
                    }
                    Ok(_) => {}
                    Err(refusal) => return Err(refusal),
                }
            }
        }
    }
    Ok(())
}

/// Every (store, node, value) the release writes: each node compiled once for the tenant, then once
/// per store.
///
/// Compile each node once for the tenant, then once per store. A refusal here is the
/// instruction's own — a menu that will not compile — and refuses the release rather than being
/// recorded at every shop. What a write composes on a store's tree when it lands — a menu's
/// `fees` — is not snapshotted: it is read from the tree when the write lands, and a snapshot
/// of it would publish over whatever fee rules are written before the moment arrives.
pub(super) async fn compile_pairs(
    tenant_id: TenantId,
    stores: &[StoreId],
    carried: &[CarriedNode<'_>],
) -> Result<Vec<(StoreId, String, serde_json::Value)>, Response> {
    let mut pairs: Vec<(StoreId, String, serde_json::Value)> = Vec::new();
    for (node, arguments) in carried {
        let plan = match node.plan(tenant_id, arguments).await {
            Ok(plan) => plan,
            Err(refusal) => return Err(refusal),
        };
        for store_id in stores {
            match plan.compile_for(*store_id).await {
                Ok(compiled) => {
                    for (key, value) in compiled {
                        pairs.push((*store_id, key, value));
                    }
                }
                Err(refusal) => return Err(refusal),
            }
        }
    }
    Ok(pairs)
}

/// Refuses a release that would make a store run a value that needs a recorded confirmation, which
/// it does not run now
/// ([ADR-0169](../../../../docs/adr/0169-a-setting-value-can-require-a-recorded-confirmation.md)
/// decision 3).
///
/// Each pair is checked against its store's tree as it stands, composed as the activator will
/// compose it. The activator checks again when the pair comes due, since the store may change
/// meanwhile.
pub(super) async fn check_confirmations<Cfg: ConfigTreeStore>(
    config_trees: &Cfg,
    tenant_id: TenantId,
    pairs: &[(StoreId, String, serde_json::Value)],
) -> Result<(), Response> {
    let register = pos_proto::settings::register();
    if register.iter().all(|setting| setting.confirm.is_none()) {
        return Ok(());
    }
    let mut trees: std::collections::BTreeMap<StoreId, Option<ConfigTreeState>> =
        std::collections::BTreeMap::new();
    for (store_id, node_key, node_value) in pairs {
        if !trees.contains_key(store_id) {
            let state = config_trees
                .load(tenant_id, *store_id)
                .await
                .map_err(|error| config_store_error_response(&error))?
                .map(|versioned| versioned.record);
            trees.insert(*store_id, state);
        }
        let state = trees.get(store_id).and_then(Option::as_ref);
        check_one(&register, state, node_key, node_value)
            .map_err(|unconfirmed| unconfirmed_response(&unconfirmed, Some(*store_id)))?;
    }
    Ok(())
}

/// Writes every pair at its store's instant, and returns how many were written.
pub(super) async fn schedule_pairs<Sch: ScheduledPublishStore>(
    scheduled: &Sch,
    (tenant_id, release_id): (TenantId, &str),
    created_by: &str,
    now_ms: i64,
    resolved: &[ResolvedStore],
    pairs: Vec<(StoreId, String, serde_json::Value)>,
) -> Result<usize, Response> {
    let instants: std::collections::BTreeMap<StoreId, i64> = resolved
        .iter()
        .filter_map(|store| store.outcome.as_ref().ok().map(|at| (store.store_id, *at)))
        .collect();
    // The clock each instant was read in, written down beside it. Without this the report can print
    // the moment but not the sentence an operator approved — "04:00 at Ginza" and the same instant
    // read in Ho Chi Minh City are one number and two different reviews (ADR-0125 §26).
    let zones: std::collections::BTreeMap<StoreId, String> = resolved
        .iter()
        .filter_map(|store| {
            store
                .timezone
                .as_ref()
                .map(|zone| (store.store_id, zone.clone()))
        })
        .collect();
    let mut written = 0_usize;
    for (store_id, node_key, node_value) in pairs {
        let Some(effective_at_ms) = instants.get(&store_id).copied() else {
            // Unreachable: every store resolved above or the release was refused. Recorded rather
            // than unwrapped, because the alternative is a panic in a request handler.
            tracing::error!(%store_id, release = %release_id, "a release pair has no instant");
            return Err(service_unavailable("releases"));
        };
        let Some(pair_id) = mint_ulid(now_ms) else {
            return Err(service_unavailable("releases"));
        };
        let publish = NewScheduledPublish {
            id: pair_id.to_string(),
            tenant_id,
            store_id,
            node_key,
            node_value,
            effective_at_ms,
            created_by: created_by.to_owned(),
            release_id: Some(release_id.to_owned()),
            resolved_timezone: zones.get(&store_id).cloned(),
        };
        if let Err(error) = scheduled.schedule(&publish).await {
            // Some pairs are already written. They are visible in the report and the release stays
            // `draft`, which is the honest state for a set that never fully landed — and cancelling
            // it withdraws what did get written.
            tracing::error!(%error, release = %release_id, written, "a release pair could not be scheduled");
            return Err(scheduled_error_response(&error));
        }
        written += 1;
    }
    Ok(written)
}

/// The stores the release reaches, explicitly or by its group, and each one's timezone.
pub(super) async fn target_stores<Grp, Reg, Cfg>(
    (groups, registry, config_trees): (&Grp, &Reg, &Cfg),
    tenant_id: TenantId,
    release: &Release,
    request: &ScheduleReleaseRequest,
) -> Result<(Vec<StoreId>, Vec<TargetStore>), Response>
where
    Grp: StoreGroupStore,
    Reg: RegistryStore,
    Cfg: ConfigTreeStore,
{
    let stores = match release_target_stores(
        groups,
        registry,
        tenant_id,
        release.target_group_id.as_deref(),
        request.store_ids.as_deref(),
    )
    .await
    {
        Ok(stores) => stores,
        Err(refusal) => return Err(refusal),
    };
    let targets = match release_target_timezones(config_trees, tenant_id, &stores).await {
        Ok(targets) => targets,
        Err(refusal) => return Err(refusal),
    };
    Ok((stores, targets))
}

/// Marks the release scheduled, records the decision, and answers with its report.
pub(super) async fn mark_scheduled<Rel, Sch, Grp, Reg, Cfg, A, C>(
    state: &ReleaseState<Rel, Sch, Grp, Reg, Cfg, A, C>,
    context: &AdminContext,
    (tenant_id, release_id): (TenantId, &str),
    summary: serde_json::Value,
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
    if let Err(error) = state
        .releases
        .set_status(tenant_id, release_id, ReleaseStatus::Scheduled)
        .await
    {
        return release_error_response(&error);
    }
    // One audit row for the decision, beside the per-publish rows the activator's writes will add:
    // "who decided to ship Tết" has one answer and "when did Ginza get it" has forty (ADR-0125 §7).
    audit_action(
        &state.audit,
        &state.clock,
        context,
        Some(tenant_id),
        "release.scheduled",
        "release",
        release_id,
        None,
        Some(summary),
    )
    .await;
    release_report_response(state, tenant_id, release_id).await
}
