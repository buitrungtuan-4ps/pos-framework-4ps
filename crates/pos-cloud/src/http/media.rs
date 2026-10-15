// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Media (`/admin/media`): upload, serve, list and delete image renditions
//! ([ADR-0075](../../../../docs/adr/0075-media-and-file-rail.md)).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::header::CONTENT_TYPE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use pos_cloud_api::console::{audit_action, mint_ulid, require_permission};
use pos_cloud_api::response::{api_error, parse_ulid_fields};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::TenantId;

use crate::audit::AuditRecorder;
use crate::auth::admin::AdminStore;
use crate::auth::console_rbac::ConsolePermission;
use crate::http::{MediaListQuery, RegistryTenantQuery, not_found, paged_ok, parse_page};
use crate::images::{self, ImagePipelineError, RenderError};
use crate::media::{MediaId, MediaStore, MediaStoreError, NewMediaAsset, Rendition};
use crate::paging::Page;

// --- Media (ADR-0075, Track M5): upload, serve, list, and delete image renditions -----------------

/// The largest upload the media route accepts before re-encoding. A generous cap on the *original*
/// bytes (the body limit rejects anything larger with `413`); the stored renditions are bounded far
/// smaller by the pipeline's ≤30 KB / ≤150 KB budgets.
const MAX_MEDIA_UPLOAD_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone)]
struct MediaState<M, A, C> {
    media: M,
    admin: A,
    clock: C,
    audit: Arc<dyn AuditRecorder>,
}

/// One media asset as listed — id, content type, the detail rendition's size, and when it was stored.
#[derive(Debug, Clone, serde::Serialize)]
struct MediaSummaryView {
    media_id: String,
    content_type: String,
    detail_bytes: u64,
    created_at_ms: i64,
}

/// The `POST /admin/media` response: the id the caller references the new asset by, and its size.
#[derive(Debug, Clone, serde::Serialize)]
struct UploadedMedia {
    media_id: String,
    detail_bytes: u64,
}

/// Builds the media sub-router ([ADR-0075](../../../../docs/adr/0075-media-and-file-rail.md), Track M5).
///
/// Four routes on the tenant's media library. `POST` uploads an image as a raw binary body (under an
/// 8 MB limit), re-encodes it through the ADR-0042 pipeline, and stores the two bounded renditions;
/// `GET /admin/media` lists summaries; `GET .../thumbnail` and `.../detail` stream one rendition as
/// `image/jpeg`; `DELETE` removes an asset. Upload and delete need [`ConsolePermission::ManageMedia`]
/// and are audited; reads and the two serve routes need only [`ConsolePermission::Read`]. The original
/// upload is never stored — only the renditions.
pub fn media_router<M, A, C>(media: M, admin: A, clock: C, audit: Arc<dyn AuditRecorder>) -> Router
where
    M: MediaStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/admin/media",
            get(admin_list_media::<M, A, C>).post(admin_upload_media::<M, A, C>),
        )
        .route(
            "/admin/media/{media_id}",
            delete(admin_delete_media::<M, A, C>),
        )
        .route(
            "/admin/media/{media_id}/thumbnail",
            get(admin_get_media_thumbnail::<M, A, C>),
        )
        .route(
            "/admin/media/{media_id}/detail",
            get(admin_get_media_detail::<M, A, C>),
        )
        .layer(axum::extract::DefaultBodyLimit::max(MAX_MEDIA_UPLOAD_BYTES))
        .with_state(MediaState {
            media,
            admin,
            clock,
            audit,
        })
}

/// A super-admin lists a tenant's media assets (summaries, without the bytes).
async fn admin_list_media<M, A, C>(
    State(state): State<MediaState<M, A, C>>,
    headers: HeaderMap,
    Query(query): Query<MediaListQuery>,
) -> Response
where
    M: MediaStore + Clone + Send + Sync + 'static,
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
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // Two reads, chosen by whether the caller named a limit (ADR-0098). The image picker attaches a
    // photograph to an item and needs the whole library to find one in; the Media screen's table
    // wants twenty-five rows and a count. Neither is a default for the other.
    let Some(page) = parse_page(query.limit.as_deref(), query.offset.as_deref()) else {
        return match state.media.list(tenant_id).await {
            Ok(rows) => (StatusCode::OK, Json(media_views(rows))).into_response(),
            Err(error) => media_error_response(&error),
        };
    };
    let page = match page {
        Ok(page) => page,
        Err(refusal) => return refusal,
    };
    match state.media.list_page(tenant_id, page).await {
        Ok(read) => paged_ok(Page::new(media_views(read.items), read.total), page),
        Err(error) => media_error_response(&error),
    }
}

/// The wire view of a batch of media summaries.
///
/// Shared by the paged and unpaged reads, so an asset cannot render one way on the table and another
/// in the picker.
fn media_views(rows: Vec<crate::media::MediaSummary>) -> Vec<MediaSummaryView> {
    rows.into_iter()
        .map(|row| MediaSummaryView {
            media_id: row.media_id.to_string(),
            content_type: row.content_type.clone(),
            detail_bytes: u64::try_from(row.detail_bytes).unwrap_or(u64::MAX),
            created_at_ms: row.created_at_ms,
        })
        .collect()
}

/// A super-admin uploads an image: it is re-encoded to two bounded JPEG renditions and stored. The raw
/// upload is never persisted. `?tenant_id=` names the owner.
async fn admin_upload_media<M, A, C>(
    State(state): State<MediaState<M, A, C>>,
    headers: HeaderMap,
    Query(query): Query<RegistryTenantQuery>,
    body: axum::body::Bytes,
) -> Response
where
    M: MediaStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageMedia,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    // The render is the most expensive thing this process does per request, so it goes to the
    // blocking pool under a concurrency cap rather than holding an async worker for the whole walk
    // (`images::render_blocking`). `body` moves into the task: it is a `Bytes`, so that is a handle,
    // not a copy of the upload.
    let renditions = match images::render_blocking(body).await {
        Ok(renditions) => renditions,
        Err(RenderError::Pipeline(ImagePipelineError::Decode(_))) => {
            return api_error(
                ErrorStatus::InvalidArgument,
                "the upload is not a decodable image",
            );
        }
        Err(RenderError::Pipeline(ImagePipelineError::Budget { .. })) => {
            // The pipeline understood the image and could not fit it in the budget: well-formed
            // request, unprocessable content (ADR-0096). There is no field to name — the upload is
            // the whole body — so this carries no `details`.
            return api_error(
                ErrorStatus::Unprocessable,
                "the image could not be reduced within the size budget",
            );
        }
        Err(RenderError::Pipeline(ImagePipelineError::Encode(_))) => {
            tracing::error!("encoding a media rendition failed");
            return api_error(ErrorStatus::Internal, "could not encode the image");
        }
        Err(RenderError::Busy { limit, wait }) => {
            // Nothing is wrong with the upload or the request; the server is at its rendering cap.
            // `RESOURCE_EXHAUSTED` is the retryable status (ADR-0096), which is the honest answer —
            // the console can send this one again in a moment and it will work.
            tracing::warn!(limit, ?wait, "refused a media upload at the rendering cap");
            return api_error(
                ErrorStatus::ResourceExhausted,
                "the server is busy rendering uploads; try this one again shortly",
            );
        }
        Err(RenderError::Join(error)) => {
            tracing::error!(%error, "the media rendering task did not return");
            return api_error(ErrorStatus::Internal, "could not render the image");
        }
    };
    let Some(media_id) =
        mint_ulid(state.clock.now().as_milliseconds_since_epoch()).map(MediaId::new)
    else {
        return media_entropy_unavailable();
    };
    let detail_bytes = u64::try_from(renditions.detail.len()).unwrap_or(u64::MAX);
    let asset = NewMediaAsset {
        media_id,
        tenant_id,
        content_type: "image/jpeg".to_owned(),
        thumbnail: renditions.thumbnail,
        detail: renditions.detail,
    };
    match state.media.put(&asset).await {
        Ok(()) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "media.upload",
                "media_asset",
                &media_id.to_string(),
                None,
                Some(serde_json::json!({ "content_type": "image/jpeg", "detail_bytes": detail_bytes })),
            )
            .await;
            (
                StatusCode::CREATED,
                Json(UploadedMedia {
                    media_id: media_id.to_string(),
                    detail_bytes,
                }),
            )
                .into_response()
        }
        Err(error) => media_error_response(&error),
    }
}

/// Streams a stored rendition as `image/jpeg`, or `404` if the tenant has no such asset.
async fn serve_media_rendition<M, A, C>(
    state: &MediaState<M, A, C>,
    headers: &HeaderMap,
    tenant: &str,
    media_id: &str,
    rendition: Rendition,
) -> Response
where
    M: MediaStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    if let Err(denied) =
        require_permission(&state.admin, &state.clock, headers, ConsolePermission::Read).await
    {
        return denied;
    }
    let tenant_id = match parse_ulid_fields([("tenant_id", tenant)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let media_id = match parse_ulid_fields([("media_id", media_id)]) {
        Ok([media_id]) => MediaId::new(media_id),
        Err(refusal) => return refusal,
    };
    match state.media.get(tenant_id, media_id, rendition).await {
        Ok(Some(bytes)) => (
            [
                (CONTENT_TYPE, "image/jpeg"),
                // Renditions are immutable (content is replaced by a new id), so cache hard, privately.
                (
                    axum::http::header::CACHE_CONTROL,
                    "private, max-age=31536000, immutable",
                ),
            ],
            bytes,
        )
            .into_response(),
        Ok(None) => not_found("media asset"),
        Err(error) => media_error_response(&error),
    }
}

/// A super-admin (or any reader) fetches an asset's thumbnail rendition.
async fn admin_get_media_thumbnail<M, A, C>(
    State(state): State<MediaState<M, A, C>>,
    headers: HeaderMap,
    Path(media_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    M: MediaStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    serve_media_rendition(
        &state,
        &headers,
        &query.tenant_id,
        &media_id,
        Rendition::Thumbnail,
    )
    .await
}

/// A super-admin (or any reader) fetches an asset's detail rendition.
async fn admin_get_media_detail<M, A, C>(
    State(state): State<MediaState<M, A, C>>,
    headers: HeaderMap,
    Path(media_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    M: MediaStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    serve_media_rendition(
        &state,
        &headers,
        &query.tenant_id,
        &media_id,
        Rendition::Detail,
    )
    .await
}

/// A super-admin deletes a media asset.
async fn admin_delete_media<M, A, C>(
    State(state): State<MediaState<M, A, C>>,
    headers: HeaderMap,
    Path(media_id): Path<String>,
    Query(query): Query<RegistryTenantQuery>,
) -> Response
where
    M: MediaStore + Clone + Send + Sync + 'static,
    A: AdminStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
{
    let context = match require_permission(
        &state.admin,
        &state.clock,
        &headers,
        ConsolePermission::ManageMedia,
    )
    .await
    {
        Ok(context) => context,
        Err(denied) => return denied,
    };
    let tenant_id = match parse_ulid_fields([("tenant_id", &query.tenant_id)]) {
        Ok([tenant_id]) => TenantId::new(tenant_id),
        Err(refusal) => return refusal,
    };
    let media_id = match parse_ulid_fields([("media_id", &media_id)]) {
        Ok([media_id]) => MediaId::new(media_id),
        Err(refusal) => return refusal,
    };
    match state.media.delete(tenant_id, media_id).await {
        Ok(true) => {
            audit_action(
                &state.audit,
                &state.clock,
                &context,
                Some(tenant_id),
                "media.delete",
                "media_asset",
                &media_id.to_string(),
                None,
                None,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => not_found("media asset"),
        Err(error) => media_error_response(&error),
    }
}

/// The `503` a media-store failure becomes.
fn media_error_response(error: &MediaStoreError) -> Response {
    tracing::error!(%error, "a media store operation failed");
    api_error(ErrorStatus::Unavailable, "the media service is unavailable")
}

/// The `503` a failure to mint a media id becomes (OS entropy unavailable).
fn media_entropy_unavailable() -> Response {
    tracing::error!("could not read OS entropy to mint a media id");
    api_error(ErrorStatus::Unavailable, "the media service is unavailable")
}
