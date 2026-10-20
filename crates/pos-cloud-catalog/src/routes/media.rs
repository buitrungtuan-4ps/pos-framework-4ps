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
use crate::auth::admin::{AdminContext, AdminStore};
use crate::auth::console_rbac::ConsolePermission;
use crate::images::{self, ImagePipelineError, RenderError, Renditions};
use crate::media::{MediaId, MediaStore, MediaStoreError, NewMediaAsset, Rendition};
use crate::paging::Page;
use crate::routes::{MediaListQuery, RegistryTenantQuery, not_found, paged_ok, parse_page};

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

/// Builds the media sub-router ([ADR-0075](../../../docs/adr/0075-media-and-file-rail.md), Track M5).
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

#[utoipa::path(
    get,
    path = "/admin/media",
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)"),
        ("limit" = Option<u32>, Query, description = "How many assets to return, 1 to 500. Absent, \
                                                    the whole library as an array rather than a page"),
        ("offset" = Option<u32>, Query, description = "How many assets to skip, 0 to 100000. Only \
                                                     with `limit`"),
    ),
    responses(
        (status = 200, description = "Without `limit`, every asset as an array; with it, a page: \
                                      `items`, `total`, `limit` and `offset`. Each asset has \
                                      `media_id`, `content_type`, `detail_bytes` and \
                                      `created_at_ms`, and no image bytes"),
        (status = 400, description = "tenant_id is absent or not a ULID, limit or offset is not an \
                                      integer in range, or offset is sent without limit", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The media store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "media",
)]
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

#[utoipa::path(
    post,
    path = "/admin/media",
    security(("session_cookie" = [])),
    params(
        ("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)"),
    ),
    request_body(
        description = "The image as the raw request body, at most 8 MB. It is re-encoded into two \
                       bounded JPEG renditions, a thumbnail and a detail image (ADR-0042, ADR-0075); \
                       the upload itself is never stored",
        content_type = "application/octet-stream",
    ),
    responses(
        (status = 201, description = "Stored. The body carries `media_id`, what an item's \
                                      `image_ref` names, and `detail_bytes`. Audited"),
        (status = 400, description = "tenant_id is absent or not a ULID, or the upload is not a \
                                      decodable image", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.media.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 413, description = "The upload is over 8 MB. Answered by the server's body limit, \
                                      without the error envelope"),
        (status = 422, description = "The image could not be reduced within the renditions' size \
                                      budget", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 429, description = "The server is at its rendering limit: send the upload again \
                                      shortly", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 500, description = "The image could not be encoded or rendered", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The media store or the entropy source is unavailable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "media",
)]
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
        Err(error) => return render_refusal(error),
    };
    store_upload(&state, &context, tenant_id, renditions).await
}

/// The answer to an upload that did not render: a bad upload, one the budget cannot hold, the
/// server at its rendering cap, or an internal fault.
fn render_refusal(error: RenderError) -> Response {
    match error {
        RenderError::Pipeline(ImagePipelineError::Decode(_)) => api_error(
            ErrorStatus::InvalidArgument,
            "the upload is not a decodable image",
        ),
        RenderError::Pipeline(ImagePipelineError::Budget { .. }) => {
            // The pipeline understood the image and could not fit it in the budget: well-formed
            // request, unprocessable content (ADR-0096). There is no field to name — the upload is
            // the whole body — so this carries no `details`.
            api_error(
                ErrorStatus::Unprocessable,
                "the image could not be reduced within the size budget",
            )
        }
        RenderError::Pipeline(ImagePipelineError::Encode(_)) => {
            tracing::error!("encoding a media rendition failed");
            api_error(ErrorStatus::Internal, "could not encode the image")
        }
        RenderError::Busy { limit, wait } => {
            // Nothing is wrong with the upload or the request; the server is at its rendering cap.
            // `RESOURCE_EXHAUSTED` is the retryable status (ADR-0096), which is the honest answer —
            // the console can send this one again in a moment and it will work.
            tracing::warn!(limit, ?wait, "refused a media upload at the rendering cap");
            api_error(
                ErrorStatus::ResourceExhausted,
                "the server is busy rendering uploads; try this one again shortly",
            )
        }
        RenderError::Join(error) => {
            tracing::error!(%error, "the media rendering task did not return");
            api_error(ErrorStatus::Internal, "could not render the image")
        }
    }
}

/// Stores an upload's renditions under a new id, records it, and answers the id and the size.
async fn store_upload<M, A, C>(
    state: &MediaState<M, A, C>,
    context: &AdminContext,
    tenant_id: TenantId,
    renditions: Renditions,
) -> Response
where
    M: MediaStore,
    C: ClockSource,
{
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
                context,
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

#[utoipa::path(
    get,
    path = "/admin/media/{media_id}/thumbnail",
    security(("session_cookie" = [])),
    params(
        ("media_id" = String, Path, description = "The asset (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The thumbnail rendition as `image/jpeg`. A rendition never \
                                      changes, since a new image is a new id, so it is cached \
                                      privately for a year"),
        (status = 400, description = "tenant_id or media_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no asset with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The media store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "media",
)]
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

#[utoipa::path(
    get,
    path = "/admin/media/{media_id}/detail",
    security(("session_cookie" = [])),
    params(
        ("media_id" = String, Path, description = "The asset (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 200, description = "The detail rendition as `image/jpeg`. A rendition never \
                                      changes, since a new image is a new id, so it is cached \
                                      privately for a year"),
        (status = 400, description = "tenant_id or media_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.data.read", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no asset with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The media store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "media",
)]
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

#[utoipa::path(
    delete,
    path = "/admin/media/{media_id}",
    security(("session_cookie" = [])),
    params(
        ("media_id" = String, Path, description = "The asset (a 26-character ULID)"),
        ("tenant_id" = String, Query, description = "The tenant (a 26-character ULID)"),
    ),
    responses(
        (status = 204, description = "Deleted, and audited"),
        (status = 400, description = "tenant_id or media_id is absent or not a ULID", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 401, description = "No session", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 403, description = "The role lacks console.media.manage", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 404, description = "The tenant has no asset with that id", body = pos_cloud_api::error_schema::ErrorResponse),
        (status = 503, description = "The media store is unreachable", body = pos_cloud_api::error_schema::ErrorResponse),
    ),
    tag = "media",
)]
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use axum::http::StatusCode;
    use axum::response::Response;

    use super::render_refusal;
    use crate::images::{ImagePipelineError, RenderError};

    fn image_error() -> image::ImageError {
        image::ImageError::Limits(image::error::LimitError::from_kind(
            image::error::LimitErrorKind::DimensionError,
        ))
    }

    /// The status and the envelope's message.
    async fn answered(response: Response) -> (StatusCode, String) {
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read the body");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("a JSON envelope");
        let message = body["error"]["message"]
            .as_str()
            .expect("a message")
            .to_owned();
        (status, message)
    }

    /// Every way a render can fail, answered as the upload route answers it. Only a bad upload is
    /// reachable over HTTP in a test; the rest are pinned here.
    #[tokio::test]
    async fn each_render_failure_has_its_own_status_and_message() {
        let join = tokio::spawn(async { panic!("a rendering task that does not return") })
            .await
            .expect_err("the task panics");
        for (error, status, message) in [
            (
                RenderError::Pipeline(ImagePipelineError::Decode(image_error())),
                StatusCode::BAD_REQUEST,
                "the upload is not a decodable image",
            ),
            (
                RenderError::Pipeline(ImagePipelineError::Budget {
                    budget: 30_000,
                    smallest: 40_000,
                }),
                StatusCode::UNPROCESSABLE_ENTITY,
                "the image could not be reduced within the size budget",
            ),
            (
                RenderError::Pipeline(ImagePipelineError::Encode(image_error())),
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not encode the image",
            ),
            (
                RenderError::Busy {
                    limit: 2,
                    wait: Duration::from_secs(5),
                },
                StatusCode::TOO_MANY_REQUESTS,
                "the server is busy rendering uploads; try this one again shortly",
            ),
            (
                RenderError::Join(join),
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not render the image",
            ),
        ] {
            assert_eq!(
                answered(render_refusal(error)).await,
                (status, message.to_owned())
            );
        }
    }
}
