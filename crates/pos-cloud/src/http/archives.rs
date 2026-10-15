// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Store archives (ADR-0124): a store's archive key, and the sealed archive it uploads
//! (`/sync/stores/{store_id}/archive-key`, `/sync/stores/{store_id}/archive`).
//!
//! The store-facing state, request and response bodies, router and handlers are here.

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use pos_cloud_api::response::{api_error, parse_ulid_fields, service_unavailable};
use pos_ports::blob_store::{BlobKey, BlobStore};
use pos_proto::ErrorStatus;
use pos_proto::determinism::ClockSource;
use pos_proto::ids::{StoreId, TenantId};
use serde::{Deserialize, Serialize};

use crate::archive::{
    ArchiveSecret, ArchiveStore, StoreArchive, StoreArchiveKey, archive_object_key,
};
use crate::auth::apikey::{ApiKeyStore, Scope};
use crate::auth::bearer::{authenticate, require_scope, require_store};
use crate::http::{Json, hex_digest};

mod console;

pub use console::store_archive_admin_router;
// The handler the console's OpenAPI document names.
pub(crate) use console::__path_admin_list_store_archives;

// ---------------------------------------------------------------------------------------------
// Store archives ([ADR-0124](../../../../docs/adr/0124-a-store-that-can-be-restored.md))
// ---------------------------------------------------------------------------------------------

/// The largest sealed archive the cloud accepts from a store.
///
/// The same number `pos_edge::backup::MAX_ARCHIVE_BYTES` refuses to *write*, so a store cannot
/// produce an archive this rejects — the two must move together, and a mismatch shows up as a
/// store whose backups have silently stopped. On the route rather than the reverse proxy, for the
/// reason the release upload gives: a fork terminating TLS elsewhere ([ADR-0090](../../../../docs/adr/0090-tls-postures.md)'s
/// `external` posture) still gets the limit.
const MAX_STORE_ARCHIVE_BYTES: usize = 256 * 1024 * 1024;

/// What the collaborators the archive routes need: the key store and clock that authenticate a
/// store, the object store the sealed bytes go to, the registry that indexes them, and the
/// box-local secret that wraps a key.
#[derive(Clone)]
struct ArchiveState<K, C, B, R> {
    keys: K,
    clock: C,
    blobs: B,
    archives: R,
    secret: ArchiveSecret,
}

/// When the store took the snapshot — its clock, not the cloud's.
///
/// Required rather than defaulted to "now": a shop that was offline for a day ships yesterday's
/// archive today, and the recovery point is when the snapshot was taken. Defaulting would quietly
/// record the wrong one, and the wrong one is the number an operator reads when deciding what they
/// have lost.
#[derive(Debug, Clone, Deserialize)]
struct ArchiveUploadQuery {
    taken_at: i64,
}

/// The key a store seals with, as it comes back over `/sync`.
#[derive(Debug, Clone, Serialize)]
struct ArchiveKeyResponse {
    key: String,
}

/// What the cloud recorded, echoed so a store can log what it shipped.
#[derive(Debug, Clone, Serialize)]
struct ArchiveAcceptedResponse {
    object_key: String,
    size_bytes: i64,
    sha256: String,
}

/// Builds the store-archive sub-router
/// ([ADR-0124](../../../../docs/adr/0124-a-store-that-can-be-restored.md)).
///
/// Merged only when **both** an object store and `archive_key_secret` are configured, exactly like
/// [`ota_artifact_router`]: with no object store there is nowhere for the bytes to go, and with no
/// secret there is nowhere safe to keep the key, so a route that always answers `503` would be
/// worse than one honestly absent. `pos-cloud`'s boot says which of the two is missing.
///
/// Two routes, both authenticated by the store's own scoped key and both held to it by
/// [`require_store`] — the same gate the report route beside them uses (S1). `Scope::ReadConfig` is
/// what a store's provisioned key carries, and it is what the whole store-facing `/sync` surface is
/// gated on; a new scope would be a scope every already-issued key lacks, which would show up as
/// archives that never start.
pub fn store_archive_router<K, C, B, R>(
    keys: K,
    clock: C,
    blobs: B,
    archives: R,
    secret: ArchiveSecret,
) -> Router
where
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    B: BlobStore + Clone + Send + Sync + 'static,
    R: ArchiveStore + Clone + Send + Sync + 'static,
{
    Router::new()
        .route(
            "/sync/stores/{store_id}/archive-key",
            post(serve_store_archive_key::<K, C, B, R>),
        )
        .route(
            "/sync/stores/{store_id}/archive",
            post(receive_store_archive::<K, C, B, R>),
        )
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_STORE_ARCHIVE_BYTES,
        ))
        .with_state(ArchiveState {
            keys,
            clock,
            blobs,
            archives,
            secret,
        })
}

/// `POST /sync/stores/{store_id}/archive-key` — the key this store seals its archives with, minted
/// on the first ask.
///
/// Mint-on-first-ask rather than an admin action, because a store that starts archiving must not
/// need somebody to have remembered; the console reveals and rotates it afterwards. The mint is
/// insert-if-absent in one statement ([`ArchiveStore::adopt_key`]), so two tills asking in the same
/// second get the *same* key rather than two — one of which would be the key to archives nobody
/// could open.
///
/// A `POST` for a value that reads like a `GET`, and deliberately so: the first call has a side
/// effect that outlives it. A `GET` that mints is one a caching proxy or a browser prefetch may
/// perform unasked, and the thing it would create here is the key to every archive this store ever
/// ships.
async fn serve_store_archive_key<K, C, B, R>(
    State(state): State<ArchiveState<K, C, B, R>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
) -> Response
where
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    B: BlobStore + Clone + Send + Sync + 'static,
    R: ArchiveStore + Clone + Send + Sync + 'static,
{
    let (grant, store_id) =
        match authorize_store(&state.keys, &state.clock, &headers, &store_id).await {
            Ok(pair) => pair,
            Err(refusal) => return refusal,
        };

    match state.archives.wrapped_key(grant.tenant(), store_id).await {
        Ok(Some(wrapped)) => match StoreArchiveKey::unwrap_from(&wrapped, &state.secret) {
            Ok(key) => Json(ArchiveKeyResponse { key: key.to_text() }).into_response(),
            Err(error) => {
                // The stored key does not open under this box's secret. Almost always
                // `archive_key_secret` was changed, which orphans every key wrapped under the old
                // one (ADR-0124 Amendment 1). `Internal`, not `Unavailable`: retrying does not fix
                // it, and telling a store to back off and try again would be a lie.
                tracing::error!(
                    %error,
                    store = %store_id,
                    "a stored archive key does not open under archive_key_secret; was the secret                      changed?"
                );
                api_error(
                    ErrorStatus::Internal,
                    "this store's archive key cannot be read with the configured                      archive_key_secret",
                )
            }
        },
        Ok(None) => mint_store_archive_key(&state, grant.tenant(), store_id).await,
        Err(error) => {
            tracing::warn!(%error, "reading the archive registry failed");
            service_unavailable("archive registry")
        }
    }
}

/// Mints, wraps and adopts a key, answering with whichever key is current afterwards.
async fn mint_store_archive_key<K, C, B, R>(
    state: &ArchiveState<K, C, B, R>,
    tenant: TenantId,
    store_id: StoreId,
) -> Response
where
    R: ArchiveStore,
    C: ClockSource,
{
    let Ok(minted) = StoreArchiveKey::mint() else {
        return api_error(
            ErrorStatus::Internal,
            "the OS entropy source is unavailable, so no archive key could be minted",
        );
    };
    let wrapped = match minted.wrap(&state.secret) {
        Ok(wrapped) => wrapped,
        Err(error) => {
            tracing::error!(%error, "wrapping a new archive key failed");
            return api_error(
                ErrorStatus::Internal,
                "the new archive key could not be wrapped for storage",
            );
        }
    };
    let minted_at = state.clock.now().as_milliseconds_since_epoch();
    // The returned wrapped key is whichever one is current — this one, or the one a concurrent
    // till adopted a moment earlier. Unwrapping *that* rather than answering with `minted` is what
    // makes the race safe rather than merely rare.
    let current = match state
        .archives
        .adopt_key(tenant, store_id, &wrapped, minted_at)
        .await
    {
        Ok(current) => current,
        Err(error) => {
            tracing::warn!(%error, "recording a new archive key failed");
            return service_unavailable("archive registry");
        }
    };
    match StoreArchiveKey::unwrap_from(&current, &state.secret) {
        Ok(key) => Json(ArchiveKeyResponse { key: key.to_text() }).into_response(),
        Err(error) => {
            tracing::error!(%error, "a just-adopted archive key does not open");
            api_error(
                ErrorStatus::Internal,
                "this store's archive key cannot be read with the configured archive_key_secret",
            )
        }
    }
}

/// `POST /sync/stores/{store_id}/archive?taken_at=` — a store ships a sealed archive.
///
/// The cloud is a **sink for bytes it cannot read**. It does not open the archive, does not parse
/// it, and holds no key material for it beyond the wrapped column — the seal happened at the till
/// precisely so that a store's buyer details ([ADR-0107](../../../../docs/adr/0107-the-buyer-is-a-subject.md))
/// never exist here in plaintext.
///
/// What it *does* check is the header: eight magic bytes and a format version, so an obviously
/// wrong body (a plain database, a truncated download, an HTML error page from a proxy) is refused
/// at upload instead of discovered at restore. That is a shape check and emphatically not
/// verification — only the key opens an archive, and the cloud has none.
async fn receive_store_archive<K, C, B, R>(
    State(state): State<ArchiveState<K, C, B, R>>,
    headers: HeaderMap,
    Path(store_id): Path<String>,
    Query(query): Query<ArchiveUploadQuery>,
    body: axum::body::Bytes,
) -> Response
where
    K: ApiKeyStore + Clone + Send + Sync + 'static,
    C: ClockSource + Clone + Send + Sync + 'static,
    B: BlobStore + Clone + Send + Sync + 'static,
    R: ArchiveStore + Clone + Send + Sync + 'static,
{
    let (grant, store_id) =
        match authorize_store(&state.keys, &state.clock, &headers, &store_id).await {
            Ok(pair) => pair,
            Err(refusal) => return refusal,
        };
    if query.taken_at <= 0 {
        return api_error(
            ErrorStatus::InvalidArgument,
            "taken_at: milliseconds since the epoch, from the store's own clock",
        );
    }
    if !looks_like_a_store_archive(&body) {
        return api_error(
            ErrorStatus::InvalidArgument,
            "this body is not a store archive; the store seals one with `pos-edge archive`",
        );
    }

    let key_text = archive_object_key(store_id, query.taken_at);
    let Ok(object_key) = BlobKey::parse(&key_text) else {
        // Unreachable: the components are a ULID and a zero-padded integer. Reported rather than
        // unwrapped, because a panic here would take the process down for a case the types nearly
        // rule out.
        return api_error(
            ErrorStatus::Internal,
            "the archive's storage key could not be built",
        );
    };
    if let Err(error) = state.blobs.put(&object_key, &body).await {
        tracing::warn!(%error, key = object_key.as_str(), "storing a store archive failed");
        return service_unavailable("object store");
    }

    let size_bytes = i64::try_from(body.len()).unwrap_or(i64::MAX);
    let sha256 = hex_digest(&body);
    let archive = StoreArchive {
        store_id,
        taken_at: query.taken_at,
        object_key: key_text,
        size_bytes,
        sha256: sha256.clone(),
        received_at: state.clock.now().as_milliseconds_since_epoch(),
    };
    if let Err(error) = state
        .archives
        .record_archive(grant.tenant(), &archive)
        .await
    {
        // The bytes are stored and the index row is not, so the archive exists but nothing knows
        // it does. `Unavailable` rather than `Internal`, because the store retrying is exactly the
        // right response: `put` is last-write-wins and `record_archive` is an upsert, so the retry
        // converges rather than duplicating.
        tracing::warn!(%error, "recording a store archive failed after the bytes were stored");
        return service_unavailable("archive registry");
    }
    tracing::info!(
        store = %store_id,
        size_bytes,
        taken_at = query.taken_at,
        "a store archive arrived"
    );
    Json(ArchiveAcceptedResponse {
        object_key: archive.object_key,
        size_bytes,
        sha256,
    })
    .into_response()
}

/// Authenticates a store's own scoped key and holds it to the store in the path.
///
/// Extracted because both archive routes need the identical four steps, and a second copy is a
/// second place for one of them to go missing.
async fn authorize_store<K, C>(
    keys: &K,
    clock: &C,
    headers: &HeaderMap,
    store_id: &str,
) -> Result<(crate::auth::apikey::Grant, StoreId), Response>
where
    K: ApiKeyStore,
    C: ClockSource,
{
    let grant = authenticate(keys, clock, headers)
        .await
        .map_err(IntoResponse::into_response)?;
    require_scope(&grant, Scope::ReadConfig).map_err(IntoResponse::into_response)?;
    let store_id = match parse_ulid_fields([("store_id", store_id)]) {
        Ok([store_id]) => StoreId::new(store_id),
        Err(refusal) => return Err(refusal),
    };
    require_store(&grant, store_id).map_err(IntoResponse::into_response)?;
    Ok((grant, store_id))
}

/// Whether these bytes begin the way `pos_edge::backup` writes an archive.
///
/// The magic and the format byte, and nothing else. It costs nine bytes to tell an operator "you
/// uploaded the wrong file" at the moment they can still fix it, rather than at a restore months
/// later; it proves nothing about the contents, which only the key can.
fn looks_like_a_store_archive(body: &[u8]) -> bool {
    const MAGIC: &[u8] = b"P4PSTORE";
    const FORMAT_VERSION: u8 = 1;
    body.get(..MAGIC.len()) == Some(MAGIC) && body.get(MAGIC.len()) == Some(&FORMAT_VERSION)
}
