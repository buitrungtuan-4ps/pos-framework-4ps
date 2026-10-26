// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `/admin/capabilities` and `/admin/config/capabilities`: the capability catalogue, and its publish
//! to a store ([ADR-0071](../../../../../docs/adr/0071-config-without-json.md)).

use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use pos_cloud_config::publish::publish_config_nodes;
use pos_cloud_config::route::PublishedConfig;

// --- Capability catalogue (`/admin/capabilities`, ADR-0071) -------------------------------------

/// One capability flag as the console's form editor reads it: its config key, default, and the
/// one-line description (§10 catalogue). Mirrors `pos-core`'s `CapabilityMeta` so the console renders
/// toggles from the framework's own source of truth rather than a hand-kept list.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct CapabilityFlagView {
    key: &'static str,
    default_on: bool,
    description: &'static str,
    /// Whether the console offers the flag as a switch: `false` for one no release reads yet
    /// ([`offered`]).
    offered: bool,
}

/// Whether the console offers `capability` as a switch
/// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
/// decision 5: every switch the console offers changes behaviour at the edge).
///
/// `false` for the four nothing reads yet: no domain rule, edge route or till screen acts on
/// `tabs_enabled`, `pay_first_enabled`, `barcode_enabled` or `queue_number_enabled`. No tab, no
/// pay-first flow and no barcode entry exists, and every tableless order is given a queue number
/// whatever the switch says. Only the console stops offering them. A store's stored value is kept,
/// published and parsed as before, the presets still name them, and the §10 rules still apply to
/// them. A flag is offered again in the change that gives it a reader.
const fn offered(capability: pos_core::capability::Capability) -> bool {
    use pos_core::capability::Capability;
    !matches!(
        capability,
        Capability::Tabs | Capability::PayFirst | Capability::Barcode | Capability::QueueNumber
    )
}

/// One capability preset (§10) — a named starting profile the console offers as a button, given as the
/// set of flag keys it turns on.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct CapabilityPresetView {
    id: &'static str,
    keys: Vec<&'static str>,
}

/// One inter-flag rule (§10) the console previews before publish, so a conflict shows the moment it is
/// created rather than as a `422` on publish.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct CapabilityRuleView {
    id: &'static str,
    description: &'static str,
}

/// The whole capability catalogue the form editor needs: the flags, the presets, and the inter-flag
/// rules — all §10 data, tenant-independent.
#[derive(Debug, Clone, serde::Serialize, utoipa::ToSchema)]
struct CapabilityCatalogueView {
    flags: Vec<CapabilityFlagView>,
    presets: Vec<CapabilityPresetView>,
    rules: Vec<CapabilityRuleView>,
}

/// The collaborators the capability-catalogue read needs: just the admin and clock its session guard
/// uses. The catalogue itself is static `pos-core` data, so there is no store to carry.
#[derive(Clone)]
struct CapabilitiesState<A, C> {
    admin: A,
    clock: C,
}

/// The keys a preset turns on, in catalogue order.
fn preset_keys(context: pos_core::capability::CapabilityContext) -> Vec<&'static str> {
    pos_core::capability::Capability::ALL
        .iter()
        .copied()
        .filter(|capability| context.enabled(*capability))
        .map(|capability| capability.meta().key)
        .collect()
}

/// Builds the capability-catalogue sub-router ([ADR-0071](../../../docs/adr/0071-config-without-json.md)).
///
/// One read, behind [`ConsolePermission::Read`] (every console role): the §10 capability flags, the
/// presets, and the inter-flag rules, so the Config screen's form editor renders toggles and previews
/// conflicts from the framework's own catalogue. Tenant-independent — the catalogue is the same for
/// every store.
pub fn capabilities_router<A, C>(admin: A, clock: C) -> Router
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/admin/capabilities", get(admin_list_capabilities::<A, C>))
        .with_state(CapabilitiesState { admin, clock })
}

#[utoipa::path(
    get,
    path = "/admin/capabilities",
    extensions(("x-permission" = json!("console.data.read"))),
    security(("session_cookie" = [])),
    responses(
        (status = 200, body = CapabilityCatalogueView, description = "The capability catalogue, the same for every tenant: `flags`, \
                                      each with its `key`, `default_on`, `description` and `offered` \
                                      (whether the console offers it as a switch); `presets`, each \
                                      an `id` and the keys it turns on; and `rules`, the inter-flag \
                                      rules a publish enforces"),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "config nodes",
)]
/// Serves the §10 capability catalogue for the console's form editor. Each flag says whether the
/// console offers it as a switch ([`offered`]).
async fn admin_list_capabilities<A, C>(
    State(state): State<CapabilitiesState<A, C>>,
    headers: HeaderMap,
) -> Response
where
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    use pos_core::capability::{Capability, CapabilityContext, RULES};

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
    let flags = Capability::ALL
        .iter()
        .copied()
        .map(|capability| {
            let meta = capability.meta();
            CapabilityFlagView {
                key: meta.key,
                default_on: meta.default_on,
                description: meta.description,
                offered: offered(capability),
            }
        })
        .collect();
    let presets = vec![
        CapabilityPresetView {
            id: "full_service",
            keys: preset_keys(CapabilityContext::full_service()),
        },
        CapabilityPresetView {
            id: "counter",
            keys: preset_keys(CapabilityContext::counter()),
        },
        CapabilityPresetView {
            id: "retail",
            keys: preset_keys(CapabilityContext::retail()),
        },
    ];
    let rules = RULES
        .iter()
        .map(|rule| CapabilityRuleView {
            id: rule.id,
            description: rule.description,
        })
        .collect();
    (
        StatusCode::OK,
        Json(CapabilityCatalogueView {
            flags,
            presets,
            rules,
        }),
    )
        .into_response()
}

// --- Capability publish (`/admin/config/capabilities`, ADR-0071) --------------------------------

/// The collaborators the capability-publish route needs: the config-tree store the flags are merged
/// onto, plus the admin/clock/audit every write carries.
#[derive(Clone)]
struct ConfigCapabilitiesState<Cfg, A, C> {
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A super-admin sets a store's capability flags from the form editor: the (tenant, store) and the flag
/// values to write. Only the flags named are changed; the rest of the store's config is untouched.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
struct PublishCapabilitiesRequest {
    tenant_id: String,
    store_id: String,
    /// The capability flag values to set, keyed by each capability's config key (`tables_enabled`, …).
    flags: std::collections::BTreeMap<String, bool>,
}

/// Builds the capability-publish sub-router ([ADR-0071](../../../docs/adr/0071-config-without-json.md)).
///
/// One route: merge a store's capability flag booleans into its Store config layer and version it
/// through the config tree — the node-merge the catalog/people publishes use, so the other Store-level
/// keys (`menu`, `layout`, `permissions`) survive. Behind [`ConsolePermission::PublishConfig`], and the
/// config tree runs the §10 inter-flag rules, so an invalid combination is a `422`, never a stored
/// state.
pub fn config_capabilities_router<Cfg, A, C>(
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
) -> Router
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/capabilities",
            axum::routing::put(admin_publish_capabilities::<Cfg, A, C>),
        )
        .with_state(ConfigCapabilitiesState {
            config_trees,
            admin,
            clock,
            audit,
        })
}

/// Turns a capability-flag map into the config nodes a publish writes, refusing an unknown key.
///
/// Every key must be a known §10 capability flag — the console form only sends catalogue keys, and
/// this keeps a typo from writing a stray boolean into the config document. Extracted so the batch
/// fan-out ([ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md) §3) validates
/// by the same rule as the single-store route rather than by a second copy of it.
///
/// # Errors
///
/// A `400` naming the `flags` field when a key is not a known capability flag.
#[expect(
    clippy::result_large_err,
    reason = "the refusal *is* an axum Response by design — the route's own, factored out so the \
              batch fan-out refuses identically rather than by a second copy of the rule"
)]
pub fn capability_flag_nodes(
    flags: &std::collections::BTreeMap<String, bool>,
) -> Result<Vec<(String, serde_json::Value)>, Response> {
    for key in flags.keys() {
        if !pos_core::capability::Capability::ALL
            .iter()
            .any(|capability| capability.meta().key == key)
        {
            return Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                format!("unknown capability flag: {key}"),
                &[("flags", "INVALID_ENUM_VALUE")],
            ));
        }
    }
    Ok(flags
        .iter()
        .map(|(key, value)| (key.clone(), serde_json::Value::Bool(*value)))
        .collect())
}

#[utoipa::path(
    put,
    path = "/admin/config/capabilities",
    extensions(("x-permission" = json!("console.config.publish"))),
    security(("session_cookie" = [])),
    request_body(
        content = PublishCapabilitiesRequest, description = "`tenant_id` and `store_id`, then `flags`, each capability key from `GET /admin/capabilities` with its value. Only the flags \
                       named are changed",
        content_type = "application/json",
    ),
    responses(
        (status = 200, body = PublishedConfig, description = "Published as the store's `capabilities` node on its \
                                      Store layer. The body carries `config_version_id`, which the \
                                      store pulls on its next sync. Audited"),
        (status = 400, description = "tenant_id or store_id is not a ULID, a key is not a capability flag, or a value needs \
                                      a recorded confirmation first (ADR-0169)", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 412, description = "Every attempt to write the store's configuration lost a race \
                                      with another publish: try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 422, description = "The store's configuration would not validate with the \
                                      node, naming each inter-flag rule it breaks", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The configuration store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "config nodes",
)]
/// Merges a store's capability flags into its Store config layer and versions it — the same
/// load→merge→publish→version shape as the catalog publish, onto the top-level flag keys instead of a
/// node. Rejects an unknown flag key (the form only sends catalogue keys). The §10 inter-flag rules run
/// in the config tree, so an invalid combination returns `422` with the violated rules.
async fn admin_publish_capabilities<Cfg, A, C>(
    State(state): State<ConfigCapabilitiesState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishCapabilitiesRequest>,
) -> Response
where
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
    // Load the store's tree, set the flag keys on its Store layer (index 2), and re-publish that layer,
    // preserving the other Store-level keys (`menu`, `layout`, `permissions`).
    let nodes = match capability_flag_nodes(&request.flags) {
        Ok(nodes) => nodes,
        Err(refusal) => return refusal,
    };

    let id = match publish_config_nodes(
        &state.config_trees,
        &state.clock,
        tenant_id,
        store_id,
        ConfigLevel::Store,
        nodes,
    )
    .await
    {
        Ok(id) => id,
        Err(refusal) => return refusal,
    };
    {
        audit_action(
            &state.audit,
            &state.clock,
            &context,
            Some(tenant_id),
            "config.capabilities.publish",
            "store",
            &store_id.to_string(),
            None,
            serde_json::to_value(&request.flags).ok(),
        )
        .await;
        (
            StatusCode::OK,
            Json(PublishedConfig {
                config_version_id: id.to_string(),
            }),
        )
            .into_response()
    }
}
