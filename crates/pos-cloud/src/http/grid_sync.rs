// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /sync/stores/{store_id}/translations`: a store pulls its tenant's translation grid whole
//! ([ADR-0043](../../../docs/adr/0043-translation-grid.md), the store-facing fetch its consequences
//! name), beside its config pull and behind the same scoped key.
//!
//! The grid holds labels HQ wrote (a menu item's name, a custom field's label, a receipt's footer),
//! never personal data, and every key carries `en`, so a store can always fall back to English.

use std::collections::BTreeMap;

use axum::Router;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pos_cloud_api::response::{parse_ulid_fields, service_unavailable};
use pos_cloud_catalog::translations::TranslationStore;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::StoreId;
use serde::Serialize;

use crate::auth::apikey::{ApiKeyStore, Scope};
use crate::auth::bearer::{authenticate, require_scope, require_store};
use crate::http::Json;

/// A store's tenant grid, `key → { locale → text }`, empty when the tenant has authored none.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub(crate) struct StoreGridResponse {
    grid: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Clone)]
struct GridSyncState<K, C, Tr> {
    keys: K,
    clock: C,
    translations: Tr,
}

/// Builds the store-facing grid pull, with its own state as the other `/sync` sub-routers have.
pub fn store_grid_router<K, C, Tr>(keys: K, clock: C, translations: Tr) -> Router
where
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Tr: TranslationStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/sync/stores/{store_id}/translations",
            get(serve_store_grid::<K, C, Tr>),
        )
        .with_state(GridSyncState {
            keys,
            clock,
            translations,
        })
}

#[utoipa::path(
    get,
    path = "/sync/stores/{store_id}/translations",
    security(("device_credential" = ["read_config"])),
    params(
        ("store_id" = String, Path, description = "The store, a ULID. It must be the credential's own"),
    ),
    responses(
        (status = 200, body = StoreGridResponse, description = "`grid`: the tenant's translation grid whole, `key → { locale → text }`, \
                                      every key with `en`; empty when the tenant has authored none (ADR-0043)"),
        (status = 400, description = "store_id is not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No credential, or one the cloud does not accept: one answer for every \
                                      credential problem", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The credential lacks the `read_config` scope, or belongs to another \
                                      store", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 429, description = "Too many requests to the store sync surface: `Retry-After` says when \
                                      to try again", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The credential store, or the translation store, is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "configuration",
)]
/// Serves a store its tenant's translation grid, whole. The tenant is the credential's, never the
/// path's.
async fn serve_store_grid<K, C, Tr>(
    State(state): State<GridSyncState<K, C, Tr>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
) -> Response
where
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    Tr: TranslationStore + Clone + Send + Sync + 'static,
{
    let grant = match authenticate(&state.keys, &state.clock, &headers).await {
        Ok(grant) => grant,
        Err(denied) => return denied.into_response(),
    };
    if let Err(forbidden) = require_scope(&grant, Scope::ReadConfig) {
        return forbidden.into_response();
    }
    let store_id = match parse_ulid_fields([("store_id", &store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return refusal,
    };
    if let Err(forbidden) = require_store(&grant, store_id) {
        return forbidden.into_response();
    }
    match state.translations.load(grant.tenant()).await {
        Ok(found) => {
            let grid = found
                .map(|versioned| versioned.record.as_map().clone())
                .unwrap_or_default();
            (StatusCode::OK, Json(StoreGridResponse { grid })).into_response()
        }
        Err(error) => {
            tracing::warn!(%error, "a store's grid pull could not read the translation store");
            service_unavailable("translation store")
        }
    }
}
