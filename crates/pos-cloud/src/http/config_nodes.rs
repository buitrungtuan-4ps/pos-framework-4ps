// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The per-store configuration nodes a console writes directly: tax, the store profile, locale,
//! and the channel, tender, origin, QR, retention and vendor settings, with the node compilers the
//! batch fan-out shares (ADR-0074, ADR-0080, ADR-0106, ADR-0122 §3).
//!
//! The channel settings' state, queries and request bodies, the shared node write and the channel
//! settings router are here.

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use channels::{
    admin_publish_channels, admin_publish_origins, admin_publish_tender, admin_read_channels,
    admin_read_origins, admin_read_tender,
};
use pos_cloud_api::console::audit_action;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::wire_enum::{Open, WireEnum};
use qr::{admin_publish_qr, admin_read_qr};
use retention::{admin_publish_retention, admin_read_retention};
use serde::Deserialize;
use vendors::{admin_publish_vendors, admin_read_vendors};

use crate::audit::AuditRecorder;
use crate::auth::admin::{AdminContext, AdminStore};
use crate::config_tree::{ConfigLevel, ConfigTreeStore};
use crate::http::{Json, PublishedConfig, accepted_tokens, enum_refusal, publish_config_nodes};

// `pub(super)`: the locale node checks in `locale_node.rs` and the unit tests in `http.rs` reach
// into these.
mod channels;
pub(super) mod compilers;
pub(super) mod locale;
pub(super) mod qr;
mod retention;
mod store_profile;
mod tax;
mod vendors;

pub use locale::config_locale_router;
pub use store_profile::config_store_profile_router;
pub use tax::config_tax_router;
// What the batch fan-out compiles each store's nodes with.
pub(super) use compilers::{
    QrGuardrailFields, channels_node, origins_node, qr_guardrail_node, tender_node,
    vendor_policies_node,
};
pub(super) use tax::tax_nodes;

// --- Channels & tender settings (`/admin/config/channels`, `/admin/config/tender`, ADR-0080, M7) --

/// The collaborators the channels/tender settings routes need: the config-tree store the node is
/// written onto and read back from, plus the admin/clock/audit every write carries.
#[derive(Clone)]
struct ConfigChannelsState<Cfg, A, C> {
    config_trees: Cfg,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// A `(tenant, store)` query for reading a store's current settings node.
#[derive(Debug, Clone, Deserialize)]
struct ConfigNodeQuery {
    tenant_id: String,
    store_id: String,
}

/// A `PUT /admin/config/channels` body: the `(tenant, store)` and the enabled sales-channel tokens.
#[derive(Debug, Clone, Deserialize)]
struct PublishChannelsRequest {
    tenant_id: String,
    store_id: String,
    #[serde(default)]
    enabled: Vec<String>,
}

/// A `PUT /admin/config/tender` body: the `(tenant, store)` and the accepted payment-method tokens.
#[derive(Debug, Clone, Deserialize)]
struct PublishTenderRequest {
    tenant_id: String,
    store_id: String,
    #[serde(default)]
    accepted: Vec<String>,
}

/// A `PUT /admin/config/origins` body: the `(tenant, store)` and the origins that store's edge
/// answers beside the one that served the request
/// ([ADR-0111](../../../docs/adr/0111-a-second-origin-may-address-the-edge.md)).
///
/// The serving origin is never listed here and never needs to be: the edge compares it against the
/// request's own `Host`, so a store that publishes nothing keeps serving its own UI.
#[derive(Debug, Clone, Deserialize)]
struct PublishOriginsRequest {
    tenant_id: String,
    store_id: String,
    #[serde(default)]
    allowed: Vec<String>,
}

/// Parses each `UPPER_SNAKE_CASE` token to a known wire-enum value, refusing an unrecognised or
/// unspecified one — authoring rejects a typo up front rather than storing a token the edge would drop.
#[expect(
    clippy::result_large_err,
    reason = "the Err is an axum Response by design — it *is* the 400 the caller returns, the shape `parse_ulid_fields` already carries this expectation for"
)]
pub(in crate::http) fn parse_known_tokens<E: WireEnum>(
    field: &str,
    tokens: &[String],
) -> Result<Vec<Open<E>>, Response> {
    let mut out = Vec::with_capacity(tokens.len());
    for token in tokens {
        match E::from_wire(token) {
            Some(known) if known != E::UNSPECIFIED => out.push(Open::from_known(known)),
            // One refusal for two causes — an unrecognised token, and an explicit `*_UNSPECIFIED`
            // that parses and is still not a choice — because the answer is the same either way:
            // here is the set you may pick from, and neither is in it. The old message
            // (`"{token} is not a recognised value"`) named the field for neither and the set for
            // no one.
            _ => return Err(enum_refusal(field, accepted_tokens::<E>())),
        }
    }
    Ok(out)
}

/// Publishes `node_value` as the store's `node_key`, versioned — the shared load→merge→publish→save→
/// audit tail every settings-node publish uses.
async fn publish_store_settings_node<Cfg, A, C>(
    state: &ConfigChannelsState<Cfg, A, C>,
    context: &AdminContext,
    tenant_id: TenantId,
    store_id: StoreId,
    node_key: &str,
    action: &str,
    node_value: serde_json::Value,
) -> Response
where
    Cfg: ConfigTreeStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let nodes = vec![(node_key.to_owned(), node_value.clone())];
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
            context,
            Some(tenant_id),
            action,
            "store",
            &store_id.to_string(),
            None,
            Some(node_value),
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

/// Builds the channels/tender settings sub-router ([ADR-0080](../../../docs/adr/0080-channels-and-payments.md), M7).
///
/// `GET`/`PUT /admin/config/channels` and `.../tender`: read a store's current enabled channels /
/// accepted tender, and publish new ones. Reads are behind [`ConsolePermission::Read`](crate::auth::console_rbac::ConsolePermission::Read); publishes are
/// behind [`ConsolePermission::PublishConfig`](crate::auth::console_rbac::ConsolePermission::PublishConfig) and audited — settings, not a CRUD master-data domain,
/// so there is no dedicated authoring permission. The edge applies each node as a policy gate.
pub fn config_channels_router<Cfg, A, C>(
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
            "/admin/config/channels",
            get(admin_read_channels::<Cfg, A, C>).put(admin_publish_channels::<Cfg, A, C>),
        )
        .route(
            "/admin/config/tender",
            get(admin_read_tender::<Cfg, A, C>).put(admin_publish_tender::<Cfg, A, C>),
        )
        .route(
            "/admin/config/qr",
            get(admin_read_qr::<Cfg, A, C>).put(admin_publish_qr::<Cfg, A, C>),
        )
        .route(
            "/admin/config/retention",
            get(admin_read_retention::<Cfg, A, C>).put(admin_publish_retention::<Cfg, A, C>),
        )
        .route(
            "/admin/config/vendors",
            get(admin_read_vendors::<Cfg, A, C>).put(admin_publish_vendors::<Cfg, A, C>),
        )
        .route(
            "/admin/config/origins",
            get(admin_read_origins::<Cfg, A, C>).put(admin_publish_origins::<Cfg, A, C>),
        )
        .with_state(ConfigChannelsState {
            config_trees,
            admin,
            clock,
            audit,
        })
}
