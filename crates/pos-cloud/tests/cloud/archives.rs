// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Store archives (ADR-0124): a store's archive key, the sealed archive it uploads, and the
//! console's read of what it has archived.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use pos_cloud::auth::admin::AdminRole;
use pos_cloud::auth::apikey::Scope;
use pos_cloud::http;
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::ulid::Ulid;
use tower::ServiceExt;

use crate::{
    FakeBlobs, FakeKeys, clock, get_with_cookie, issue_key, issue_store_key, json_body,
    provisioned_admin, role_session_cookie, store_id, tenant,
};

// ---------------------------------------------------------------------------------------------
// ADR-0124: a store's archive key, and an archive the cloud cannot read
// ---------------------------------------------------------------------------------------------

/// The archive registry, holding whatever a test recorded.
#[derive(Clone, Default)]
struct FakeArchives {
    keys: Arc<Mutex<HashMap<String, String>>>,
    archives: Arc<Mutex<Vec<(TenantId, pos_cloud::archive::StoreArchive)>>>,
}

impl pos_cloud::archive::ArchiveStore for FakeArchives {
    async fn wrapped_key(
        &self,
        tenant: TenantId,
        store: StoreId,
    ) -> Result<Option<String>, pos_cloud::archive::ArchiveStoreError> {
        Ok(self
            .keys
            .lock()
            .expect("the key map is not poisoned")
            .get(&format!("{tenant}/{store}"))
            .cloned())
    }

    async fn adopt_key(
        &self,
        tenant: TenantId,
        store: StoreId,
        wrapped: &str,
        _minted_at: i64,
    ) -> Result<String, pos_cloud::archive::ArchiveStoreError> {
        // Insert-if-absent, like the real statement: the entry that is already there wins.
        Ok(self
            .keys
            .lock()
            .expect("the key map is not poisoned")
            .entry(format!("{tenant}/{store}"))
            .or_insert_with(|| wrapped.to_owned())
            .clone())
    }

    async fn record_archive(
        &self,
        tenant: TenantId,
        archive: &pos_cloud::archive::StoreArchive,
    ) -> Result<(), pos_cloud::archive::ArchiveStoreError> {
        self.archives
            .lock()
            .expect("the archive list is not poisoned")
            .push((tenant, archive.clone()));
        Ok(())
    }

    async fn list_archives(
        &self,
        tenant: TenantId,
        store: StoreId,
        limit: i64,
    ) -> Result<Vec<pos_cloud::archive::StoreArchive>, pos_cloud::archive::ArchiveStoreError> {
        let archives = self
            .archives
            .lock()
            .expect("the archive list is not poisoned");
        let mut matching: Vec<pos_cloud::archive::StoreArchive> = archives
            .iter()
            .filter(|(row_tenant, archive)| *row_tenant == tenant && archive.store_id == store)
            .map(|(_tenant, archive)| archive.clone())
            .collect();
        // Newest first, like the real adapter's `ORDER BY taken_at DESC`.
        matching.sort_by(|left, right| right.taken_at.cmp(&left.taken_at));
        matching.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
        Ok(matching)
    }

    async fn expired_before(
        &self,
        cutoff: i64,
        limit: i64,
    ) -> Result<Vec<pos_cloud::archive::ExpiredArchive>, pos_cloud::archive::ArchiveStoreError>
    {
        let archives = self
            .archives
            .lock()
            .expect("the archive list is not poisoned");
        Ok(archives
            .iter()
            .filter(|(_tenant, archive)| archive.taken_at <= cutoff)
            .take(usize::try_from(limit).unwrap_or(usize::MAX))
            .map(|(tenant, archive)| pos_cloud::archive::ExpiredArchive {
                tenant: *tenant,
                store_id: archive.store_id,
                taken_at: archive.taken_at,
                object_key: archive.object_key.clone(),
            })
            .collect())
    }

    async fn forget_archive(
        &self,
        tenant: TenantId,
        store: StoreId,
        taken_at: i64,
    ) -> Result<(), pos_cloud::archive::ArchiveStoreError> {
        self.archives
            .lock()
            .expect("the archive list is not poisoned")
            .retain(|(row_tenant, archive)| {
                !(*row_tenant == tenant
                    && archive.store_id == store
                    && archive.taken_at == taken_at)
            });
        Ok(())
    }
}

fn archive_secret() -> pos_cloud::archive::ArchiveSecret {
    pos_cloud::archive::ArchiveSecret::new(
        "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff",
    )
}

fn store_archive_app(archives: FakeArchives, blobs: FakeBlobs) -> (axum::Router, FakeKeys) {
    let keys = FakeKeys::default();
    let router =
        http::store_archive_router(keys.clone(), clock(), blobs, archives, archive_secret());
    (router, keys)
}

/// Bytes shaped like what `pos_edge::backup` seals: the magic, the format byte, and filler.
///
/// Deliberately not a real archive — the cloud cannot open one, and a test that needed a real one
/// would be testing the edge's cipher through the wrong door.
fn archive_bytes() -> Vec<u8> {
    let mut body = b"P4PSTORE".to_vec();
    body.push(1);
    body.extend_from_slice(&[0_u8; 64]);
    body
}

/// A `POST` with an empty JSON body, for the archive-key route.
///
/// A `POST` for what reads like a read, because the first call *mints* the key (ADR-0124): a `GET`
/// with that side effect is one a caching proxy or a browser prefetch may perform unasked.
fn mint_bearer(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/json")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from("{}"))
        .expect("build the request")
}

fn post_bytes_bearer(uri: &str, bytes: Vec<u8>, token: &str) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("content-type", "application/octet-stream")
        .header("authorization", format!("Bearer {token}"))
        .body(Body::from(bytes))
        .expect("build the request")
}

/// The first ask mints; the second returns the same key. A store that seals under two different
/// keys has archives it cannot open, so this is the property that matters most.
#[tokio::test]
async fn a_store_asking_twice_gets_the_same_archive_key() {
    let archives = FakeArchives::default();
    let (router, keys) = store_archive_app(archives.clone(), FakeBlobs::default());
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);
    let path = format!("/sync/stores/{}/archive-key", store_id());

    let first = router
        .clone()
        .oneshot(mint_bearer(&path, &token))
        .await
        .expect("route the first ask");
    assert_eq!(first.status(), StatusCode::OK);
    let first_key = json_body(first).await["key"]
        .as_str()
        .expect("a key")
        .to_owned();
    assert_eq!(first_key.len(), 64, "64 hexadecimal characters");

    let second = router
        .oneshot(mint_bearer(&path, &token))
        .await
        .expect("route the second ask");
    assert_eq!(second.status(), StatusCode::OK);
    assert_eq!(
        json_body(second).await["key"].as_str(),
        Some(first_key.as_str()),
        "the same key, not a second one"
    );
}

/// What the registry holds is the wrapped key, and it is not the key
/// ([ADR-0124](../../docs/adr/0124-a-store-that-can-be-restored.md) Amendment 1). The stored column
/// travels off-box in a `pg_dump`; the key it wraps must not travel with it.
#[tokio::test]
async fn the_stored_column_is_not_the_key_the_store_receives() {
    let archives = FakeArchives::default();
    let (router, keys) = store_archive_app(archives.clone(), FakeBlobs::default());
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);

    let issued = router
        .oneshot(mint_bearer(
            &format!("/sync/stores/{}/archive-key", store_id()),
            &token,
        ))
        .await
        .expect("route the ask");
    let key = json_body(issued).await["key"]
        .as_str()
        .expect("a key")
        .to_owned();

    let stored = archives
        .keys
        .lock()
        .expect("the key map is not poisoned")
        .values()
        .next()
        .cloned()
        .expect("a stored key");
    assert_ne!(stored, key);
    assert!(
        !stored.contains(&key),
        "the wrapped column must not contain the key it wraps"
    );
}

/// A sealed archive lands in the object store under its own shop's prefix, and the row records the
/// instant the *store* took it rather than the instant the cloud received it.
#[tokio::test]
async fn a_sealed_archive_is_stored_and_indexed_under_its_own_store() {
    let archives = FakeArchives::default();
    let blobs = FakeBlobs::default();
    let (router, keys) = store_archive_app(archives.clone(), blobs.clone());
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);

    let accepted = router
        .oneshot(post_bytes_bearer(
            &format!("/sync/stores/{}/archive?taken_at=1700000000000", store_id()),
            archive_bytes(),
            &token,
        ))
        .await
        .expect("route the upload");
    assert_eq!(accepted.status(), StatusCode::OK);
    let body = json_body(accepted).await;
    let object_key = body["object_key"].as_str().expect("an object key");
    assert!(
        object_key.starts_with(&format!("stores/{}/archives/", store_id())),
        "an archive is filed under its own store: {object_key}"
    );

    let stored = blobs
        .objects
        .lock()
        .expect("the blob map is not poisoned")
        .get(object_key)
        .cloned()
        .expect("the bytes are in the object store");
    assert_eq!(stored, archive_bytes(), "byte for byte, unopened");

    let recorded = archives
        .archives
        .lock()
        .expect("the archive list is not poisoned")
        .clone();
    assert_eq!(recorded.len(), 1);
    let (recorded_tenant, archive) = &recorded[0];
    assert_eq!(*recorded_tenant, tenant(), "the tenant came from the key");
    assert_eq!(archive.store_id, store_id());
    assert_eq!(
        archive.taken_at, 1_700_000_000_000,
        "when the store took it, not when this arrived"
    );
    assert_eq!(
        archive.size_bytes,
        i64::try_from(archive_bytes().len()).expect("fits")
    );
    assert_eq!(archive.sha256.len(), 64);
}

/// The nine-byte shape check: a body that is not an archive is refused while an operator can still
/// fix it, rather than discovered at a restore months later.
#[tokio::test]
async fn a_body_that_is_not_an_archive_is_refused_before_it_is_stored() {
    let blobs = FakeBlobs::default();
    let (router, keys) = store_archive_app(FakeArchives::default(), blobs.clone());
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);

    let refused = router
        .oneshot(post_bytes_bearer(
            &format!("/sync/stores/{}/archive?taken_at=1700000000000", store_id()),
            b"SQLite format 3\0and then some".to_vec(),
            &token,
        ))
        .await
        .expect("route the upload");
    // `InvalidArgument` — a malformed request, not a well-formed one the state refuses.
    assert_eq!(refused.status(), StatusCode::BAD_REQUEST);
    assert!(
        blobs
            .objects
            .lock()
            .expect("the blob map is not poisoned")
            .is_empty(),
        "and nothing was stored"
    );
}

/// `taken_at` is required and must be a real instant: a shop offline for a day ships yesterday's
/// archive today, and a defaulted or zero value would record the wrong recovery point — which is
/// the number an operator reads when deciding what they have lost.
#[tokio::test]
async fn an_upload_without_a_believable_taken_at_is_refused() {
    let (router, keys) = store_archive_app(FakeArchives::default(), FakeBlobs::default());
    let token = issue_store_key(&keys, tenant(), store_id(), &[Scope::ReadConfig]);

    let missing = router
        .clone()
        .oneshot(post_bytes_bearer(
            &format!("/sync/stores/{}/archive", store_id()),
            archive_bytes(),
            &token,
        ))
        .await
        .expect("route the upload");
    assert!(
        missing.status().is_client_error(),
        "taken_at is not optional: {}",
        missing.status()
    );

    let zero = router
        .oneshot(post_bytes_bearer(
            &format!("/sync/stores/{}/archive?taken_at=0", store_id()),
            archive_bytes(),
            &token,
        ))
        .await
        .expect("route the upload");
    assert_eq!(zero.status(), StatusCode::BAD_REQUEST);
}

/// The console read: what a store has shipped, newest first, without a key going anywhere near it.
#[tokio::test]
async fn the_console_sees_what_a_store_has_archived_newest_first() {
    let archives = FakeArchives::default();
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Viewer, "viewer-archives").await;
    for taken_at in [1_700_000_000_000_i64, 1_700_086_400_000] {
        pos_cloud::archive::ArchiveStore::record_archive(
            &archives,
            tenant(),
            &pos_cloud::archive::StoreArchive {
                store_id: store_id(),
                taken_at,
                object_key: pos_cloud::archive::archive_object_key(store_id(), taken_at),
                size_bytes: 4_096,
                sha256: "00".repeat(32),
                received_at: taken_at + 60_000,
            },
        )
        .await
        .expect("record an archive");
    }
    let router = http::store_archive_admin_router(archives, admin, clock());

    let listed = router
        .oneshot(get_with_cookie(
            &format!(
                "/admin/stores/{}/archives?tenant_id={}",
                store_id(),
                tenant()
            ),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(
        listed.status(),
        StatusCode::OK,
        "console.data.read is every role, so a Viewer can see whether a shop is backing up"
    );
    let body = json_body(listed).await;
    let rows = body["archives"].as_array().expect("a list");
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows[0]["taken_at_ms"].as_i64(),
        Some(1_700_086_400_000),
        "newest first, so the first row is the recovery point"
    );
    assert!(
        rows[0]["object_key"]
            .as_str()
            .is_some_and(|key| key.starts_with(&format!("stores/{}/archives/", store_id()))),
        "the object key an operator types into a restore is here"
    );
    assert!(
        body.to_string().find("key\":\"0123").is_none(),
        "and nothing that could open an archive is: this is the index, not the key"
    );
}

/// A store that has never archived is a `200` with an empty list, not a `404`. It is a real answer
/// and the one an operator most needs stated plainly, rather than as a missing page.
#[tokio::test]
async fn a_store_that_has_never_archived_reads_as_an_empty_list() {
    let admin = provisioned_admin();
    let cookie = role_session_cookie(&admin, AdminRole::Viewer, "viewer-none").await;
    let router = http::store_archive_admin_router(FakeArchives::default(), admin, clock());

    let listed = router
        .oneshot(get_with_cookie(
            &format!(
                "/admin/stores/{}/archives?tenant_id={}",
                store_id(),
                tenant()
            ),
            &cookie,
        ))
        .await
        .expect("route the read");
    assert_eq!(listed.status(), StatusCode::OK);
    assert_eq!(
        json_body(listed).await["archives"].as_array().map(Vec::len),
        Some(0)
    );
}

/// The console read is a console door, not a store's: an unauthenticated caller is refused.
#[tokio::test]
async fn the_console_archive_read_needs_a_session() {
    let router =
        http::store_archive_admin_router(FakeArchives::default(), provisioned_admin(), clock());
    let refused = router
        .oneshot(
            Request::get(format!(
                "/admin/stores/{}/archives?tenant_id={}",
                store_id(),
                tenant()
            ))
            .body(Body::empty())
            .expect("build"),
        )
        .await
        .expect("route the read");
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
}

/// Both routes are the store's own door: no key is `401`, the wrong scope is `403`, and a key
/// scoped to one shop cannot act for another (S1).
#[tokio::test]
async fn the_archive_routes_are_a_stores_own_door() {
    let (router, keys) = store_archive_app(FakeArchives::default(), FakeBlobs::default());
    let path = format!("/sync/stores/{}/archive-key", store_id());

    let unauthenticated = router
        .clone()
        .oneshot(Request::post(&path).body(Body::from("{}")).expect("build"))
        .await
        .expect("route the unauthenticated ask");
    assert_eq!(unauthenticated.status(), StatusCode::UNAUTHORIZED);

    let wrong_scope = issue_key(&keys, tenant(), &[Scope::PlaceOrders]);
    let refused = router
        .clone()
        .oneshot(mint_bearer(&path, &wrong_scope))
        .await
        .expect("route the wrongly scoped ask");
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);

    // A key scoped to a *different* store must not fetch this one's key — the key opens that
    // shop's archives, and its buyer details are inside them.
    let other_store = StoreId::new(Ulid::from_u128(0xB0FF));
    let neighbour = issue_store_key(&keys, tenant(), other_store, &[Scope::ReadConfig]);
    let confined = router
        .oneshot(mint_bearer(&path, &neighbour))
        .await
        .expect("route the neighbour's ask");
    assert_eq!(confined.status(), StatusCode::FORBIDDEN);
}
