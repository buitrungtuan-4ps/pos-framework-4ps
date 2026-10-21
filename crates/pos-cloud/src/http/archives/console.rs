// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `GET /admin/stores/{store_id}/archives`: what a store has archived, newest first.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use pos_cloud_api::console::require_permission;
use pos_cloud_api::response::{parse_ulid_fields, service_unavailable};
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};

use crate::archive::ArchiveStore;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{Json, RegistryTenantQuery};

// --- The console's view of what a store has archived (ADR-0124 slice 3) ------------------------

/// How many archives one console read returns.
///
/// The window is measured in weeks and archives are daily, so this comfortably covers a whole
/// window and then some. It is a cap rather than a page: the question this answers is "what can I
/// restore", and an operator scrolling past sixty backups is not the case to design for.
const MAX_ARCHIVES_LISTED: i64 = 60;

/// What the console-facing archive read needs.
#[derive(Clone)]
struct ArchiveReadState<R, A, C> {
    archives: R,
    admin: A,
    clock: C,
}

/// One archive as the console sees it.
///
/// No key and nothing that could open one — this is the *index*, and the key is a separate,
/// deliberate ask. `object_key` is here because it is what an operator types into a restore.
#[derive(Debug, serde::Serialize)]
struct ArchiveView {
    /// When the store took the snapshot, Unix ms. **This is the recovery point** — not
    /// `received_at_ms`, which is when a possibly-delayed upload landed.
    taken_at_ms: i64,
    /// When the cloud received it, Unix ms. The gap between the two is how far behind a store's
    /// uplink was.
    received_at_ms: i64,
    /// How many bytes, sealed.
    size_bytes: i64,
    /// The cloud's hex SHA-256 of the sealed bytes as they arrived, so a download can be checked
    /// before anybody spends time on a key.
    sha256: String,
    /// Where the bytes are in the object store.
    object_key: String,
}

/// The answer: what this store has, newest first.
#[derive(Debug, serde::Serialize)]
struct StoreArchivesResponse {
    archives: Vec<ArchiveView>,
}

/// Builds the console-facing archive read
/// ([ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md) slice 3).
///
/// Mounted **unconditionally**, unlike [`store_archive_router`](crate::http::store_archive_router), and that is the decision: the
/// store-facing routes need an object store and `archive_key_secret` to do anything at all, but
/// this one needs only the registry. An operator whose `archive_key_secret` has gone missing is
/// exactly the operator who most needs to see what archives exist — hiding the list along with the
/// routes would take the evidence away at the moment it matters.
pub fn store_archive_admin_router<R, A, C>(archives: R, admin: A, clock: C) -> Router
where
    R: ArchiveStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/stores/{store_id}/archives",
            get(admin_list_store_archives::<R, A, C>),
        )
        .with_state(ArchiveReadState {
            archives,
            admin,
            clock,
        })
}

#[utoipa::path(
    get,
    path = "/admin/stores/{store_id}/archives",
    security(("session_cookie" = [])),
    extensions(("x-permission" = json!("console.data.read"))),
    params(
        ("store_id" = String, Path, description = "The store whose archives to list (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant the store belongs to (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "This store's sealed archives, newest first — the first row's \
                                      taken_at_ms is the recovery point. An empty list is this \
                                      answer too: the store has never archived"),
        (status = 400, description = "tenant_id or store_id is absent or not a ULID", body = crate::openapi_admin::ErrorResponse),
        (status = 401, description = "No session", body = crate::openapi_admin::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = crate::openapi_admin::ErrorResponse),
        (status = 503, description = "The archive registry is unreachable", body = crate::openapi_admin::ErrorResponse),
    ),
    tag = "fleet",
)]
/// `GET /admin/stores/{store_id}/archives?tenant_id=` — what this store has shipped, newest first.
///
/// Behind [`ConsolePermission::Read`], the same gate the fleet view uses: "when did this store last
/// back up" is an operational fact every console role should be able to see, and a Viewer who
/// cannot see that a shop has not archived in a fortnight is a Viewer who cannot raise it.
async fn admin_list_store_archives<R, A, C>(
    State(state): State<ArchiveReadState<R, A, C>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    R: ArchiveStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
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
    let (tenant, store) =
        match parse_ulid_fields([("tenant_id", &query.tenant_id), ("store_id", &store_id)]) {
            Ok([tenant, store]) => (TenantId::new(tenant), StoreId::new(store)),
            Err(refusal) => return refusal,
        };
    match state
        .archives
        .list_archives(tenant, store, MAX_ARCHIVES_LISTED)
        .await
    {
        // An empty list is a `200`, not a `404`: a store that has never archived is a real and
        // important answer, and the one an operator most needs to see plainly.
        Ok(archives) => Json(StoreArchivesResponse {
            archives: archives
                .into_iter()
                .map(|archive| ArchiveView {
                    taken_at_ms: archive.taken_at,
                    received_at_ms: archive.received_at,
                    size_bytes: archive.size_bytes,
                    sha256: archive.sha256,
                    object_key: archive.object_key,
                })
                .collect(),
        })
        .into_response(),
        Err(error) => {
            tracing::error!(%error, "reading a store's archives failed");
            service_unavailable("archive registry")
        }
    }
}
