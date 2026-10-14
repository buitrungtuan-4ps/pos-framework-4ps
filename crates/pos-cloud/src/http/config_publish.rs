// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Publishing configuration: the machinery every config publish shares, the generic preview,
//! version history and rollback, and config releases (ADR-0069, ADR-0125).
//!
//! The shared machinery is here: a tree loaded for a conditional write, the nodes written onto a
//! layer, and the prerequisite and settings checks a write runs.

use axum::response::Response;
use pos_cloud_api::response::{api_error, service_unavailable};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};

use crate::config_tree::{
    CapabilityValidator, ConfigError, ConfigLevel, ConfigTree, ConfigTreeState, ConfigTreeStore,
    prerequisites_for,
};
use crate::http::setting_refusal::{ConfirmCheck, confirmed};
use crate::http::{
    config_store_error_response, mint_version_id, unprocessable_violations, version_mismatch,
};
use crate::version::{UpdateOutcome, Version, Versioned};

mod config_releases;
mod if_match;
mod preview;
// `pub(super)`: the redaction unit tests in `http.rs` reach the priced-node list and the credential
// strip.
pub(super) mod redaction;
mod release_actions;
mod release_create;
mod store_config;
mod versions;

pub use config_releases::config_release_router;
pub use preview::config_preview_router;
// What the other areas' routes, the router and the release schedule use.
pub(super) use config_releases::{
    ReleaseNodeRequest, ReleaseState, ScheduleReleaseRequest, release_error_response,
};
pub(super) use if_match::{if_match_config, if_match_lease};
pub(super) use preview::preview_config_node;
pub(super) use redaction::{staff_redacted_for, without_prices};
pub(super) use release_actions::release_report_response;
// The raw config-tree write and read the main router mounts.
pub(super) use release_create::{
    release_target_stores, release_target_timezones, unresolvable_moment,
};
pub(super) use store_config::{admin_config_effective, admin_config_publish};
pub(super) use versions::{
    admin_config_nodes, admin_config_rollback, admin_config_version_effective,
    admin_config_versions,
};
// The handlers the console's OpenAPI document names.
pub(crate) use release_actions::{
    __path_admin_cancel_release, __path_admin_list_releases, __path_admin_read_release,
    __path_admin_schedule_release,
};
pub(crate) use release_create::__path_admin_create_release;
pub(crate) use versions::__path_admin_config_nodes;

/// Drops the version a config-tree read was handed, keeping the state.
///
/// A read of the tree wants the four layers and the history; the version the row was read at is a
/// *writer's* precondition ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
/// Read paths say so by calling this, rather than silently discarding a field.
pub(crate) fn strip_tree_version(
    loaded: Option<Versioned<ConfigTreeState>>,
) -> Option<ConfigTreeState> {
    loaded.map(|versioned| versioned.record)
}

/// A store's config tree as it was read, with the version the store handed out.
///
/// The two travel together because a conditional write needs both: the state to compose the next
/// publish from, and the version to write against.
pub(in crate::http) struct LoadedTree {
    pub(in crate::http) state: Option<ConfigTreeState>,
    pub(in crate::http) version: Option<Version>,
}

/// Loads a store's tree for a write, keeping the version it was read at.
///
/// Split from [`publish_store_layer`] so each route can compose its own layer in between, which is
/// the only part of the twelve node-publish routes that actually differs.
pub(in crate::http) async fn load_tree_for_write<Cfg>(
    config_trees: &Cfg,
    tenant_id: TenantId,
    store_id: StoreId,
) -> Result<LoadedTree, Response>
where
    Cfg: ConfigTreeStore,
{
    match config_trees.load(tenant_id, store_id).await {
        Ok(Some(versioned)) => Ok(LoadedTree {
            state: Some(versioned.record),
            version: Some(versioned.etag),
        }),
        Ok(None) => Ok(LoadedTree {
            state: None,
            version: None,
        }),
        Err(error) => Err(config_store_error_response(&error)),
    }
}

/// How many times a write that composes on a read will re-read and re-apply before giving up.
///
/// Bounded so a pathological hot store cannot spin. Three is far beyond what a console's publish
/// rate can lose; exhausting them means the caller is contending with something other than a racing
/// operator, and a `412` is then the honest answer
/// ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
pub(in crate::http) const CONDITIONAL_WRITE_ATTEMPTS: u8 = 3;

/// Sets `nodes` on a store's tree at `level` and saves it, retrying if another publish lands first.
///
/// Twelve routes used to hand-roll the load→compose→publish→save cycle, and each copy was a place
/// the conditional write could be forgotten. It is one function now
/// ([ADR-0095](../../../docs/adr/0095-conditional-writes-for-collections.md)).
///
/// # Why this retries instead of refusing
///
/// A layer is a **map of nodes**, and this writes only the keys in `nodes`. Two publishes of
/// *different* nodes — a menu and a floor, say — do not conflict in any way an operator would
/// recognise: nothing either of them authored is lost. What the old code lost was the *document*,
/// because each publish wrote back a whole tree composed from a stale read.
///
/// So the precondition here is the store's, evaluated at write time, and losing it is not the
/// caller's problem: reload, re-apply this write's own keys onto what is now there, and save again.
/// Both nodes land and neither operator sees a conflict. Refusing instead would hand someone a
/// "the configuration changed" error for an edit that never touched their key, which is friction
/// with no correctness behind it.
///
/// Concurrent edits to the *same* node are a different question, and they are answered a layer up:
/// the authoring records a node is compiled from are themselves conditionally written (slices 2–4),
/// so two people cannot silently overwrite each other's catalog or floor edits. By the time a
/// publish runs, it is compiling committed data.
///
/// The two routes where a human edits the document itself — `PUT /admin/config` and the rollback —
/// are the exception and go through [`publish_authored_layer`] instead.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response by design — the same shape every \
              other route helper carries, and this closure never takes the Err branch at all"
)]
pub(in crate::http) async fn publish_config_nodes<Cfg, C>(
    config_trees: &Cfg,
    clock: &C,
    tenant_id: TenantId,
    store_id: StoreId,
    level: ConfigLevel,
    nodes: Vec<(String, serde_json::Value)>,
) -> Result<ConfigVersionId, Response>
where
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    // A node whose value does not depend on what the tree already holds: the closure ignores its
    // argument. Every node publish but one is this shape.
    publish_config_nodes_with(config_trees, clock, tenant_id, store_id, level, |_| {
        Ok(nodes.clone())
    })
    .await
}

/// [`publish_config_nodes`] for a node whose new value is a function of its **current** value.
///
/// `compose` is called **inside** the conditional-write retry, with the tree as just read, and its
/// result is what gets published. That is the whole point of the variant: a node authored by
/// read-merge-append is only safe under retry if the merge re-reads. Building the merged value once,
/// outside the loop, and re-publishing it on each attempt would silently drop a concurrent
/// append — two admins each retiring one till would end with one of the two retirements gone, which
/// for a deny-list ([ADR-0118](../../../docs/adr/0118-one-credential-per-box-and-the-cloud-learns.md) §6)
/// is a device left trading that somebody was told had been retired.
///
/// `compose` may refuse: returning `Err` abandons the publish with that response, which is how a
/// route reports a refusal it can only decide once it has seen the current node.
///
/// A write to the Store layer that sets QR ordering's switch, the `qr` node or the `channels` list
/// also carries what the other two need to follow the switch ([`crate::qr_ordering::align`],
/// ADR-0160 decision 5), composed here on the tree as just read, so a retry composes again.
pub(crate) async fn publish_config_nodes_with<Cfg, C, F>(
    config_trees: &Cfg,
    clock: &C,
    tenant_id: TenantId,
    store_id: StoreId,
    level: ConfigLevel,
    compose: F,
) -> Result<ConfigVersionId, Response>
where
    Cfg: ConfigTreeStore,
    C: ClockSource,
    F: FnMut(Option<&ConfigTreeState>) -> Result<Vec<(String, serde_json::Value)>, Response>,
{
    let at = (tenant_id, store_id, level);
    publish_nodes(config_trees, clock, at, ConfirmCheck::Required, compose).await
}

/// [`publish_config_nodes_with`], saying whether the write takes ADR-0169 decision 3's check. Only
/// the settings republish skips it: every value it publishes passed decision 2 when written.
pub(in crate::http) async fn publish_nodes<Cfg, C, F>(
    config_trees: &Cfg,
    clock: &C,
    (tenant_id, store_id, level): (TenantId, StoreId, ConfigLevel),
    check: ConfirmCheck,
    mut compose: F,
) -> Result<ConfigVersionId, Response>
where
    Cfg: ConfigTreeStore,
    C: ClockSource,
    F: FnMut(Option<&ConfigTreeState>) -> Result<Vec<(String, serde_json::Value)>, Response>,
{
    let mut last_conflict = false;
    for _ in 0..CONDITIONAL_WRITE_ATTEMPTS {
        let loaded = load_tree_for_write(config_trees, tenant_id, store_id).await?;
        let nodes = compose(loaded.state.as_ref())?;
        let nodes = if level == ConfigLevel::Store {
            crate::qr_ordering::align(loaded.state.as_ref(), nodes)
        } else {
            nodes
        };
        let layer = layer_with_nodes(loaded.state.as_ref(), level, &nodes);
        let mut tree = match loaded.state {
            Some(existing) => ConfigTree::from_state(store_id, CapabilityValidator, existing),
            None => ConfigTree::new(store_id, CapabilityValidator),
        };
        let Some(version_id) = mint_version_id(clock.now().as_milliseconds_since_epoch()) else {
            tracing::error!("could not read OS entropy to mint a config version id");
            return Err(service_unavailable("configuration"));
        };
        // The keys this publish set, recorded on the version so the console can say which node is
        // stale rather than only which tree is (F6).
        let node_keys: Vec<String> = nodes.iter().map(|(key, _)| key.clone()).collect();
        let before = tree.effective();
        check_prerequisites(&before, &node_keys)?;
        let id = match tree.publish(level, layer, version_id, node_keys) {
            Ok(id) => id,
            Err(ConfigError::Invalid(violations)) => {
                return Err(unprocessable_violations(&violations));
            }
        };
        confirmed(check, &before, &tree.effective())?;
        match config_trees
            .save(tenant_id, store_id, &tree.state(), loaded.version.as_ref())
            .await
        {
            Ok(UpdateOutcome::Updated(_)) => return Ok(id),
            Ok(UpdateOutcome::VersionMismatch | UpdateOutcome::NotFound) => {
                last_conflict = true;
                tracing::debug!("a concurrent config publish won; re-applying this node");
            }
            Err(error) => return Err(config_store_error_response(&error)),
        }
    }
    if last_conflict {
        tracing::warn!(
            "a config publish lost {CONDITIONAL_WRITE_ATTEMPTS} races in a row and gave up"
        );
    }
    Err(version_mismatch())
}

/// Refuses a publish whose node needs one the store has not got yet ([ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md) §7, finding **F7**).
///
/// Checked against the **effective** document — every layer merged as they stand — because that is
/// what the store receives: a `tax` node inherited from the tenant layer satisfies a menu's
/// prerequisite exactly as a store-level one does, and refusing it would be refusing a correctly
/// configured shop. The layers rather than the last published version's document, because the
/// question is what the store *holds*: a tree whose layers were set by a restore or a fixture has
/// the nodes even where no history entry names them, and the sync path composes those same layers.
///
/// The batch path asks the same question one notch narrower — [`publish_batch_member`] reads the
/// store's own layer, so a cohort member inheriting `tax` from its tenant is skipped with a reason
/// rather than published to. That is the stricter of the two and it runs first, so a batch never
/// reaches a refusal here; the one table both read is what finding **F7** was about.
///
/// A publish that writes its own prerequisite in the same call satisfies it: the four nodes a wizard
/// writes in one go should not have to be four calls in a particular order to get past a rule meant
/// to stop a *missing* node, not a simultaneous one.
///
/// The refusal names the node and what it is waiting for, because "the configuration is invalid" is
/// the message that sent the operator to the logs.
///
/// A node that carries nothing but settings is not held ([`only_settings`]): the settings compile
/// writes `locale.tax_rounding` on the Tenant layer of a store whose locale was never published.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the refusal the caller returns"
)]
pub(in crate::http) fn check_prerequisites(
    effective_before: &serde_json::Value,
    publishing: &[String],
) -> Result<(), Response> {
    let held = |key: &str| {
        effective_before.as_object().is_some_and(|map| {
            map.get(key)
                .is_some_and(|value| !value.is_null() && !only_settings(key, value))
        })
    };
    for node in publishing {
        for need in prerequisites_for(node) {
            if held(need) || publishing.iter().any(|other| other == need) {
                continue;
            }
            return Err(api_error(
                ErrorStatus::Unprocessable,
                format!(
                    "`{node}` cannot be published to this store until `{need}` is: \
                     publish `{need}` first"
                ),
            ));
        }
    }
    Ok(())
}

/// Whether `value`, the store's `node`, carries only fields the settings register writes there
/// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)).
///
/// Such a node is the settings and nothing else: a `locale` holding `tax_rounding` alone has no
/// currency, timezone or cutoff, so it is not the locale a menu waits for.
pub(in crate::http) fn only_settings(node: &str, value: &serde_json::Value) -> bool {
    let register = pos_proto::settings::register();
    value.as_object().is_some_and(|fields| {
        !fields.is_empty()
            && fields.keys().all(|field| {
                register
                    .iter()
                    .any(|setting| setting.node == node && setting.field == field)
            })
    })
}

/// Sets `nodes` on the layer at `level`, preserving every other key.
///
/// A missing or non-object prior layer starts from an empty object, so a store's first publish and a
/// corrupt layer both compose cleanly.
pub(in crate::http) fn layer_with_nodes(
    state_before: Option<&ConfigTreeState>,
    level: ConfigLevel,
    nodes: &[(String, serde_json::Value)],
) -> serde_json::Value {
    let mut layer = state_before.map_or_else(
        || serde_json::Value::Object(serde_json::Map::new()),
        |existing| existing.layer(level).clone(),
    );
    if !layer.is_object() {
        layer = serde_json::Value::Object(serde_json::Map::new());
    }
    if let serde_json::Value::Object(map) = &mut layer {
        for (key, value) in nodes {
            map.insert(key.clone(), value.clone());
        }
    }
    layer
}
