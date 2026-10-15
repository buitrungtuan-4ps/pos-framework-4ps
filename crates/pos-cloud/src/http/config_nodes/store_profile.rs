// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `PUT /admin/config/store-profile`: what a store's receipts say about it (ADR-0106).

use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use pos_cloud_api::console::{audit_action, require_permission};
use pos_cloud_api::response::{api_error_with_details, parse_ulid_fields, service_unavailable};
use pos_country::CountryRegistry;
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::locale::CountryCode;
use pos_proto::store_profile::StoreProfile;
use serde::Deserialize;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::http::{Json, PublishedConfig, publish_config_nodes};

// --- Store profile publish (`/admin/config/store-profile`, ADR-0106) ----------------------------

/// A `PUT /admin/config/store-profile` body: the `(tenant, store)` and who that store legally is.
///
/// Every field is text the operator supplies. A registered address is written differently in every
/// country, so the framework imposes no shape on it; the one field with a checkable shape is the tax
/// registration number, and the check is the **country module's**
/// ([ADR-0106](../../../../../docs/adr/0106-the-store-is-a-legal-person.md)).
#[derive(Debug, Clone, Deserialize)]
struct PublishStoreProfileRequest {
    tenant_id: String,
    store_id: String,
    /// The registered name — what the law wants on the paper.
    #[serde(default)]
    legal_name: String,
    #[serde(default)]
    trading_name: Option<String>,
    #[serde(default)]
    address_lines: Vec<String>,
    #[serde(default)]
    tax_registration_number: Option<String>,
    #[serde(default)]
    tax_registration_label: Option<String>,
    #[serde(default)]
    contact_lines: Vec<String>,
    #[serde(default)]
    footer_lines: Vec<String>,
    /// Which country's rule to check the registration number's *shape* against, as an ISO 3166-1
    /// alpha-2 code. Optional: a fork whose country module is not compiled into this cloud still
    /// publishes a profile, with the number stored unchecked rather than the publish refused.
    #[serde(default)]
    country_code: Option<String>,
}

/// The collaborators the store-profile publish needs.
#[derive(Clone)]
struct ConfigStoreProfileState<Cfg, A, C> {
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    countries: Arc<CountryRegistry>,
}

/// Builds the store-profile publish sub-router ([ADR-0106](../../../../../docs/adr/0106-the-store-is-a-legal-person.md)).
///
/// One route, behind [`ConsolePermission::PublishConfig`]: write the store's registered identity as
/// its `store_profile` config node. The edge applies it to `EdgeSession::profile` and the receipt is
/// composed from it — which is what turns the store's paper into a document a Japanese or Indian
/// auditor accepts.
pub fn config_store_profile_router<Cfg, A, C>(
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
    countries: Arc<CountryRegistry>,
) -> Router
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/config/store-profile",
            axum::routing::put(admin_publish_store_profile::<Cfg, A, C>),
        )
        .with_state(ConfigStoreProfileState {
            config_trees,
            admin,
            clock,
            audit,
            countries,
        })
}

/// Trims a field, and treats blank as absent — a store that typed a space must not get a receipt
/// with a blank line where its address should be.
fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

/// Trims a list of printed lines and drops the blank ones.
fn trimmed_lines(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .map(|line| line.trim().to_owned())
        .filter(|line| !line.is_empty())
        .collect()
}

/// Validates and writes a store's registered identity as its `store_profile` node.
async fn admin_publish_store_profile<Cfg, A, C>(
    State(state): State<ConfigStoreProfileState<Cfg, A, C>>,
    headers: HeaderMap,
    Json(request): Json<PublishStoreProfileRequest>,
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

    let registration = trimmed(request.tax_registration_number.as_deref());
    // Format only, never registration: whether a number *exists* is a call to a tax authority and
    // belongs behind `Fiscalization` (ADR-0027). This catches the typo that would otherwise reach an
    // invoice — and only when this cloud carries the country's module, so a fork serving a market it
    // has not written a pack for still publishes.
    if let (Some(number), Some(module)) = (
        registration.as_deref(),
        trimmed(request.country_code.as_deref())
            .and_then(|code| CountryCode::parse(&code).ok())
            .and_then(|code| state.countries.get(code)),
    ) && !module.is_valid_tax_code(number)
    {
        return api_error_with_details(
            ErrorStatus::InvalidArgument,
            format!(
                "tax_registration_number is not the shape {} issues",
                module.display_name()
            )
            .as_str(),
            &[("tax_registration_number", "INVALID_FORMAT")],
        );
    }

    let profile = StoreProfile {
        legal_name: request.legal_name.trim().to_owned(),
        trading_name: trimmed(request.trading_name.as_deref()),
        address_lines: trimmed_lines(&request.address_lines),
        tax_registration_number: registration,
        tax_registration_label: trimmed(request.tax_registration_label.as_deref()),
        contact_lines: trimmed_lines(&request.contact_lines),
        footer_lines: trimmed_lines(&request.footer_lines),
    };
    let Ok(profile_value) = serde_json::to_value(&profile) else {
        tracing::error!("could not serialise a store profile");
        return service_unavailable("config-tree");
    };

    let nodes = vec![("store_profile".to_owned(), profile_value)];
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
    audit_action(
        &state.audit,
        &state.clock,
        &context,
        Some(tenant_id),
        "config.store_profile.publish",
        "store",
        &store_id.to_string(),
        None,
        // The registered identity is the store's own business data, not a person's, so the trail
        // records what was published — which is the point of an audit trail on a legal document.
        serde_json::to_value(&profile).ok(),
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
