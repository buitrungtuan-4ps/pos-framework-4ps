// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `POST /admin/settings/publish`: a tenant's settings published to the stores they reach.

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error, api_error_with_details, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};
use pos_proto::settings::SettingScope;

use super::presets::SettingsStoreRequest;
use super::{SettingsState, publish_results_response, setting_scope_refusal};
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeState, ConfigTreeStore};
use crate::http::setting_refusal::ConfirmCheck;
use crate::http::{
    load_tree_for_write, not_found, publish_nodes, registry_error_response,
    store_group_error_response,
};
use crate::registry::{BrandId, EntityStatus, RegistryStore, StoreRecord};
use crate::settings::SettingsStore;
use crate::store_groups::{StoreGroup, StoreGroupId, StoreGroupStore};

#[utoipa::path(
    post,
    path = "/admin/settings/publish",
    request_body(
        description = "The tenant, and optionally one store. Without a store, every store of the \
                       tenant is republished",
        content_type = "application/json",
    ),
    responses(
        (status = 200, description = "Each store's resolved settings republished, with the same \
                                      per-store outcomes as a write"),
        (status = 400, description = "tenant_id or store_id is not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.config.publish", body = crate::openapi_admin::ErrorResponse),
        (status = 404, description = "The tenant has no such store", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The settings store, the registry, the store groups or the \
                                      config tree is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "settings",
)]
/// `POST /admin/settings/publish` — republishes the settings of one store, or of every store of the
/// tenant.
///
/// A write republishes every store it reaches by itself. What it cannot see is a store that moved:
/// a store given another brand, or added to or removed from a store group, keeps the values it was
/// last published until this runs for it, as a group's batch publish works
/// ([ADR-0122](../../../docs/adr/0122-a-store-group-is-a-delivery-cohort.md)).
pub(super) async fn admin_publish_settings<St, Rg, Grp, Cfg, A, C>(
    State(state): State<SettingsState<St, Rg, Grp, Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<SettingsStoreRequest>,
) -> Response
where
    St: SettingsStore + Clone + Send + Sync + 'static,
    Rg: RegistryStore + Clone + Send + Sync + 'static,
    Grp: StoreGroupStore + Clone + Send + Sync + 'static,
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
    let layout = match TenantLayout::load(&state.registry, &state.groups, tenant_id).await {
        Ok(layout) => layout,
        Err(refusal) => return refusal,
    };
    let stores: Vec<StoreId> = match request.store_id.as_deref().map(str::trim) {
        None | Some("") => layout.stores.iter().map(|store| store.store_id).collect(),
        Some(text) => {
            let store_id = match parse_ulid_fields([("store_id", text)]) {
                Ok([store_id]) => StoreId::new(store_id),
                Err(refusal) => return refusal,
            };
            if layout.placement(store_id).is_none() {
                return not_found("store");
            }
            vec![store_id]
        }
    };
    let results = publish_settings_to(&state, tenant_id, &layout, &stores).await;
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "setting.publish",
        "tenant",
        &tenant_id.to_string(),
        None,
        Some(serde_json::json!({ "stores": stores.len() })),
    )
    .await;
    publish_results_response(&results)
}

/// Where a tenant's stores sit — their brands and their active store groups — read once per
/// request, so one write resolves every store it reaches against the same picture.
pub(in crate::http) struct TenantLayout {
    tenant_id: TenantId,
    stores: Vec<StoreRecord>,
    brand_ids: Vec<BrandId>,
    /// Every group of the tenant with its members, archived ones included so a write to one can be
    /// told apart from a write to a group that does not exist.
    groups: Vec<(StoreGroup, Vec<StoreId>)>,
}

impl TenantLayout {
    /// Reads the tenant's stores, brands and store groups.
    pub(in crate::http) async fn load<Rg, Grp>(
        registry: &Rg,
        groups: &Grp,
        tenant_id: TenantId,
    ) -> Result<Self, Response>
    where
        Rg: RegistryStore,
        Grp: StoreGroupStore,
    {
        let stores = registry
            .list_stores(tenant_id)
            .await
            .map_err(|error| registry_error_response(&error))?
            .into_iter()
            .map(|store| store.record)
            .collect();
        let brand_ids = registry
            .list_brands(tenant_id)
            .await
            .map_err(|error| registry_error_response(&error))?
            .into_iter()
            .map(|brand| brand.record.brand_id)
            .collect();
        let mut with_members = Vec::new();
        for group in groups
            .list_groups(tenant_id)
            .await
            .map_err(|error| store_group_error_response(&error))?
        {
            let members = groups
                .list_members(tenant_id, group.record.group_id)
                .await
                .map_err(|error| store_group_error_response(&error))?;
            with_members.push((group.record, members));
        }
        Ok(Self {
            tenant_id,
            stores,
            brand_ids,
            groups: with_members,
        })
    }

    /// Where one store sits, or `None` if the tenant has no such store.
    pub(in crate::http) fn placement(
        &self,
        store_id: StoreId,
    ) -> Option<crate::settings::StorePlacement> {
        let store = self
            .stores
            .iter()
            .find(|store| store.store_id == store_id)?;
        Some(crate::settings::StorePlacement {
            tenant_id: self.tenant_id.to_string(),
            brand_id: store.brand_id.map(|brand| brand.to_string()),
            store_group_ids: self
                .groups
                .iter()
                .filter(|(group, members)| {
                    group.status == EntityStatus::Active && members.contains(&store_id)
                })
                .map(|(group, _)| group.group_id.to_string())
                .collect(),
            store_id: store_id.to_string(),
        })
    }

    /// The canonical id a value at `scope` is written for, refusing one the tenant does not have.
    #[expect(
        clippy::result_large_err,
        reason = "the Err is an axum Response by design — it is the refusal the caller returns"
    )]
    pub(super) fn scope_target(
        &self,
        scope: SettingScope,
        scope_id: &str,
    ) -> Result<String, Response> {
        let id = match parse_ulid_fields([("scope_id", scope_id)]) {
            Ok([id]) => id,
            Err(refusal) => return Err(refusal),
        };
        match scope {
            SettingScope::Tenant if id == self.tenant_id.as_ulid() => Ok(id.to_string()),
            SettingScope::Tenant => Err(api_error_with_details(
                ErrorStatus::InvalidArgument,
                "a tenant-scope value is written for the tenant itself",
                &[("scope_id", "MUST_BE_THE_TENANT")],
            )),
            SettingScope::Brand if self.brand_ids.contains(&BrandId::new(id)) => Ok(id.to_string()),
            SettingScope::Brand => Err(not_found("brand")),
            SettingScope::StoreGroup => {
                match self
                    .groups
                    .iter()
                    .find(|(group, _)| group.group_id == StoreGroupId::new(id))
                {
                    Some((group, _)) if group.status == EntityStatus::Active => Ok(id.to_string()),
                    Some(_) => Err(api_error(
                        ErrorStatus::FailedPrecondition,
                        "the store group is archived, so its values reach no store",
                    )),
                    None => Err(not_found("store group")),
                }
            }
            SettingScope::Store if self.placement(StoreId::new(id)).is_some() => Ok(id.to_string()),
            SettingScope::Store => Err(not_found("store")),
            SettingScope::Unspecified => Err(setting_scope_refusal()),
        }
    }

    /// The stores a value at `scope` reaches.
    pub(super) fn reached_by(&self, scope: SettingScope, scope_id: &str) -> Vec<StoreId> {
        match scope {
            SettingScope::Tenant => self.stores.iter().map(|store| store.store_id).collect(),
            SettingScope::Brand => self
                .stores
                .iter()
                .filter(|store| {
                    store
                        .brand_id
                        .is_some_and(|brand| brand.to_string() == scope_id)
                })
                .map(|store| store.store_id)
                .collect(),
            // A membership row for a store the registry does not have reaches nothing, as a
            // group's batch publish skips it.
            SettingScope::StoreGroup => self
                .groups
                .iter()
                .filter(|(group, _)| group.group_id.to_string() == scope_id)
                .flat_map(|(_, members)| members.iter().copied())
                .filter(|member| self.placement(*member).is_some())
                .collect(),
            SettingScope::Store => self
                .stores
                .iter()
                .filter(|store| store.store_id.to_string() == scope_id)
                .map(|store| store.store_id)
                .collect(),
            SettingScope::Unspecified => Vec::new(),
        }
    }
}

/// How one store's settings publish went.
#[derive(Debug, Clone, serde::Serialize)]
pub(super) struct SettingPublishResult {
    store_id: String,
    /// `SETTING_PUBLISH_APPLIED`, `SETTING_PUBLISH_UNCHANGED` or `SETTING_PUBLISH_FAILED`.
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_version_id: Option<String>,
}

/// Republishes each store's resolved settings onto its Tenant layer.
///
/// One store failing does not stop the others: each answers for itself, and a failed one is
/// retried with `POST /admin/settings/publish`. A store whose layer already holds what it resolves
/// to is not given a new version, so a write that changes nothing for a store does not make its
/// tills reload.
///
/// The values are read once, before the stores are written. Two writes racing for one store can
/// therefore leave it holding the earlier write's picture until its next publish; the values
/// themselves are stored correctly, and `POST /admin/settings/publish` brings the store level.
pub(super) async fn publish_settings_to<St, Rg, Grp, Cfg, A, C>(
    state: &SettingsState<St, Rg, Grp, Cfg, A, C>,
    tenant_id: TenantId,
    layout: &TenantLayout,
    stores: &[StoreId],
) -> Vec<SettingPublishResult>
where
    St: SettingsStore,
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let values = match state.settings.list(tenant_id).await {
        Ok(values) => values,
        Err(error) => {
            tracing::error!(%error, "the settings store could not be read for a settings publish");
            return stores
                .iter()
                .map(|store_id| SettingPublishResult {
                    store_id: store_id.to_string(),
                    outcome: "SETTING_PUBLISH_FAILED",
                    config_version_id: None,
                })
                .collect();
        }
    };
    let mut results = Vec::with_capacity(stores.len());
    // Stores left with the permissions switch and no people node beside it, counted and never
    // named ([`crate::settings::switch_without_people`]).
    let mut switch_only = 0_usize;
    for store_id in stores {
        let resolved = layout
            .placement(*store_id)
            .map(|placement| crate::settings::resolve_for_store(&values, &placement))
            .unwrap_or_default();
        let published = publish_settings_to_store(state, tenant_id, *store_id, &resolved).await;
        if published.switch_without_people {
            switch_only = switch_only.saturating_add(1);
        }
        results.push(match published.version {
            Ok(Some(version)) => SettingPublishResult {
                store_id: store_id.to_string(),
                outcome: "SETTING_PUBLISH_APPLIED",
                config_version_id: Some(version.to_string()),
            },
            Ok(None) => SettingPublishResult {
                store_id: store_id.to_string(),
                outcome: "SETTING_PUBLISH_UNCHANGED",
                config_version_id: None,
            },
            Err(()) => SettingPublishResult {
                store_id: store_id.to_string(),
                outcome: "SETTING_PUBLISH_FAILED",
                config_version_id: None,
            },
        });
    }
    if switch_only > 0 {
        tracing::warn!(
            stores = switch_only,
            "the permissions switch reached stores with no people node beside it: an edge from \
             0.14.1 on keeps its roster there, and an older one empties it until the store's \
             people are published (ADR-0158, ADR-0160)"
        );
    }
    results
}

/// What one store's settings publish did.
struct StorePublish {
    /// The new version, `None` when the layer already held the store's settings, or `Err` when the
    /// store could not be read or refused the write.
    version: Result<Option<ConfigVersionId>, ()>,
    /// Whether the store is left with the permissions switch and no people node beside it.
    switch_without_people: bool,
}

/// Writes one store's resolved settings onto its Tenant layer: the new version, or `None` when the
/// layer already holds them.
#[expect(
    clippy::result_large_err,
    reason = "the composing closure's Err is an axum Response by design — the shape every other \
              node publish's closure carries — and this closure never takes the Err branch"
)]
async fn publish_settings_to_store<St, Rg, Grp, Cfg, A, C>(
    state: &SettingsState<St, Rg, Grp, Cfg, A, C>,
    tenant_id: TenantId,
    store_id: StoreId,
    resolved: &[crate::settings::ResolvedSetting],
) -> StorePublish
where
    Cfg: ConfigTreeStore,
    C: ClockSource,
{
    let tenant_layer = |tree: Option<&ConfigTreeState>| {
        tree.map_or(serde_json::Value::Null, |tree| {
            tree.layer(ConfigLevel::Tenant).clone()
        })
    };
    let Ok(loaded) = load_tree_for_write(&state.config_trees, tenant_id, store_id).await else {
        tracing::error!(%store_id, "a store's configuration could not be read for a settings publish");
        return StorePublish {
            version: Err(()),
            switch_without_people: false,
        };
    };
    let layer = tenant_layer(loaded.state.as_ref());
    let nodes = crate::settings::tenant_layer_nodes(&layer, resolved);
    let switch_without_people = crate::settings::switch_without_people(
        &nodes,
        loaded
            .state
            .as_ref()
            .map_or(&serde_json::Value::Null, |tree| {
                tree.layer(ConfigLevel::Store)
            }),
    );
    if nodes
        .iter()
        .all(|(node, value)| layer.get(node) == Some(value))
    {
        return StorePublish {
            version: Ok(None),
            switch_without_people,
        };
    }
    let version = match publish_nodes(
        &state.config_trees,
        &state.clock,
        (tenant_id, store_id, ConfigLevel::Tenant),
        ConfirmCheck::SettingsRepublish,
        |tree| {
            Ok(crate::settings::tenant_layer_nodes(
                &tenant_layer(tree),
                resolved,
            ))
        },
    )
    .await
    {
        Ok(version) => Ok(Some(version)),
        Err(_refused) => {
            tracing::warn!(%store_id, "a store's configuration refused a settings publish");
            Err(())
        }
    };
    StorePublish {
        version,
        switch_without_people,
    }
}
