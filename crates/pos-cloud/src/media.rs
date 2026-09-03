// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The media store seam (Track M5, [ADR-0075](../../../docs/adr/0075-media-and-file-rail.md)).
//!
//! An uploaded image becomes two JPEG renditions under hard byte budgets (the ADR-0042 pipeline,
//! [`crate::images::render`]); this seam is where they land. Per ADR-0042 / ADR-0031 they live in a
//! Postgres `bytea` table (`media_assets`), not the condemned `blob-garage` port. The seam keeps the
//! domain shape — a minted [`MediaId`], the content type, the two renditions — and the `store-postgres`
//! `PostgresMedia` adapter provides the SQL, bridged in `crate::persistence`. Media is immutable: an
//! asset is put once, its renditions read one at a time, listed as summaries (never shipping the
//! bytes), and deleted; there is no update. All operations are tenant-scoped.

use core::fmt;
use core::future::Future;

use pos_proto::ids::TenantId;
use pos_proto::ulid::Ulid;

use crate::paging::{Page, PageRequest};

/// A media asset's identifier — a ULID minted at upload. Cloud-only: an item or brand references it
/// (`image_ref`), but it never crosses the edge wire, so it lives beside the seam like
/// [`MenuId`](crate::catalog::Menu) rather than in `pos-proto`. Serializes as its bare ULID string, so
/// an item's/brand's `image_ref` is a plain id (or `null`) on the admin API.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize)]
pub struct MediaId(Ulid);

impl MediaId {
    /// Wraps a ULID as a media id.
    #[must_use]
    pub const fn new(ulid: Ulid) -> Self {
        Self(ulid)
    }

    /// The underlying ULID.
    #[must_use]
    pub const fn as_ulid(self) -> Ulid {
        self.0
    }
}

impl fmt::Display for MediaId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Which rendition to read — the small thumbnail or the larger detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rendition {
    /// The ≤30 KB thumbnail.
    Thumbnail,
    /// The ≤150 KB detail rendition.
    Detail,
}

/// An asset to store: its id, owner, content type, and the two renditions the pipeline produced.
#[derive(Debug, Clone)]
pub struct NewMediaAsset {
    /// The minted id.
    pub media_id: MediaId,
    /// The owning tenant.
    pub tenant_id: TenantId,
    /// The stored content type (`image/jpeg` today).
    pub content_type: String,
    /// The thumbnail JPEG.
    pub thumbnail: Vec<u8>,
    /// The detail JPEG.
    pub detail: Vec<u8>,
}

/// One asset as listed — identity and size, never the bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSummary {
    /// The asset id.
    pub media_id: MediaId,
    /// The stored content type.
    pub content_type: String,
    /// The detail rendition's size in bytes.
    pub detail_bytes: usize,
    /// When the asset was stored, epoch milliseconds.
    pub created_at_ms: i64,
}

/// Persists and reads media renditions, tenant-scoped.
pub trait MediaStore {
    /// Stores one asset's two renditions.
    fn put(
        &self,
        asset: &NewMediaAsset,
    ) -> impl Future<Output = Result<(), MediaStoreError>> + Send;

    /// Reads one rendition's bytes, or `None` if the tenant has no such asset.
    fn get(
        &self,
        tenant_id: TenantId,
        media_id: MediaId,
        rendition: Rendition,
    ) -> impl Future<Output = Result<Option<Vec<u8>>, MediaStoreError>> + Send;

    /// Lists a tenant's assets, newest first, without their bytes.
    ///
    /// Every asset, unpaged. Kept as it is and not deprecated
    /// ([ADR-0098](../../../docs/adr/0098-paged-admin-reads.md)): `components/ImagePicker.tsx` reads
    /// this to offer a tenant's images when an operator attaches one to an item, and a picker showing
    /// the first twenty-five of a library cannot find the picture you want.
    fn list(
        &self,
        tenant_id: TenantId,
    ) -> impl Future<Output = Result<Vec<MediaSummary>, MediaStoreError>> + Send;

    /// One page of a tenant's assets, newest first, with the size of the whole library.
    ///
    /// Beside [`list`](Self::list) rather than replacing it: the Media screen's table wants a page
    /// and the image picker wants the library, and those are different questions.
    ///
    /// The order is `created_at DESC, media_id DESC` — total, per ADR-0098 decision 9. `created_at`
    /// alone is not: it defaults to `now()`, which is *transaction* time, so every asset uploaded in
    /// one transaction shares it exactly and `LIMIT`/`OFFSET` over the tie could return a row on two
    /// pages or on neither.
    fn list_page(
        &self,
        tenant_id: TenantId,
        page: PageRequest,
    ) -> impl Future<Output = Result<Page<MediaSummary>, MediaStoreError>> + Send;

    /// Deletes one asset, returning whether a row was removed.
    fn delete(
        &self,
        tenant_id: TenantId,
        media_id: MediaId,
    ) -> impl Future<Output = Result<bool, MediaStoreError>> + Send;
}

/// A media-store failure — the database was unreachable or a row did not decode. Carries no image
/// bytes and no personal data, only a reason for the log.
#[derive(Debug, thiserror::Error)]
#[error("the media store failed: {0}")]
pub struct MediaStoreError(String);

impl MediaStoreError {
    /// Wraps a reason.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::{MediaId, MediaStore, MediaStoreError, MediaSummary, NewMediaAsset, Rendition};
    use pos_proto::ids::TenantId;
    use pos_proto::ulid::Ulid;

    use crate::paging::{Page, PageRequest};

    #[derive(Default)]
    struct FakeMedia {
        assets: Mutex<Vec<NewMediaAsset>>,
    }

    impl MediaStore for FakeMedia {
        async fn put(&self, asset: &NewMediaAsset) -> Result<(), MediaStoreError> {
            self.assets.lock().expect("lock").push(asset.clone());
            Ok(())
        }

        async fn get(
            &self,
            tenant_id: TenantId,
            media_id: MediaId,
            rendition: Rendition,
        ) -> Result<Option<Vec<u8>>, MediaStoreError> {
            Ok(self
                .assets
                .lock()
                .expect("lock")
                .iter()
                .find(|asset| asset.tenant_id == tenant_id && asset.media_id == media_id)
                .map(|asset| match rendition {
                    Rendition::Thumbnail => asset.thumbnail.clone(),
                    Rendition::Detail => asset.detail.clone(),
                }))
        }

        async fn list(&self, tenant_id: TenantId) -> Result<Vec<MediaSummary>, MediaStoreError> {
            Ok(self.summaries(tenant_id))
        }

        async fn list_page(
            &self,
            tenant_id: TenantId,
            page: PageRequest,
        ) -> Result<Page<MediaSummary>, MediaStoreError> {
            let matching = self.summaries(tenant_id);
            let total = u32::try_from(matching.len()).unwrap_or(u32::MAX);
            let items = matching
                .into_iter()
                .skip(page.offset() as usize)
                .take(page.limit() as usize)
                .collect();
            Ok(Page::new(items, total))
        }

        async fn delete(
            &self,
            tenant_id: TenantId,
            media_id: MediaId,
        ) -> Result<bool, MediaStoreError> {
            let mut assets = self.assets.lock().expect("lock");
            let before = assets.len();
            assets.retain(|asset| !(asset.tenant_id == tenant_id && asset.media_id == media_id));
            Ok(assets.len() < before)
        }
    }

    fn tenant(n: u128) -> TenantId {
        TenantId::new(Ulid::from_u128(n))
    }

    fn media(n: u128) -> MediaId {
        MediaId::new(Ulid::from_u128(n))
    }

    fn asset(tenant_n: u128, id_n: u128) -> NewMediaAsset {
        NewMediaAsset {
            media_id: media(id_n),
            tenant_id: tenant(tenant_n),
            content_type: "image/jpeg".to_owned(),
            thumbnail: vec![1, 2, 3],
            detail: vec![4, 5, 6, 7],
        }
    }

    impl FakeMedia {
        /// The tenant's assets as summaries, in insertion order.
        ///
        /// Shared by both reads so the paged one cannot filter differently from the unpaged one — a
        /// divergence no test comparing only lengths would catch.
        fn summaries(&self, tenant_id: TenantId) -> Vec<MediaSummary> {
            self.assets
                .lock()
                .expect("lock")
                .iter()
                .filter(|asset| asset.tenant_id == tenant_id)
                .map(|asset| MediaSummary {
                    media_id: asset.media_id,
                    content_type: asset.content_type.clone(),
                    detail_bytes: asset.detail.len(),
                    created_at_ms: 0,
                })
                .collect()
        }
    }

    #[tokio::test]
    async fn put_then_get_returns_the_right_rendition_and_stays_tenant_scoped() {
        let store = FakeMedia::default();
        store.put(&asset(1, 500)).await.expect("put");
        store.put(&asset(2, 999)).await.expect("put neighbour");

        assert_eq!(
            store
                .get(tenant(1), media(500), Rendition::Thumbnail)
                .await
                .expect("get"),
            Some(vec![1, 2, 3]),
        );
        assert_eq!(
            store
                .get(tenant(1), media(500), Rendition::Detail)
                .await
                .expect("get"),
            Some(vec![4, 5, 6, 7]),
        );
        assert_eq!(
            store
                .get(tenant(2), media(500), Rendition::Detail)
                .await
                .expect("get"),
            None,
            "another tenant cannot read this asset by id",
        );

        let listed = store.list(tenant(1)).await.expect("list");
        assert_eq!(listed.len(), 1, "a tenant lists only its own assets");
        assert_eq!(listed.first().expect("row").detail_bytes, 4);
    }

    #[tokio::test]
    async fn delete_removes_only_the_named_asset() {
        let store = FakeMedia::default();
        store.put(&asset(1, 500)).await.expect("put");
        assert!(store.delete(tenant(1), media(500)).await.expect("delete"));
        assert!(
            !store.delete(tenant(1), media(500)).await.expect("delete"),
            "deleting an absent asset reports no row removed",
        );
        assert!(store.list(tenant(1)).await.expect("list").is_empty());
    }

    /// Stores `count` assets for one tenant, with ids that make a page's contents identifiable.
    async fn stored(store: &FakeMedia, tenant_n: u128, count: u128) {
        for index in 0..count {
            store
                .put(&asset(tenant_n, 1_000 + index))
                .await
                .expect("put");
        }
    }

    #[tokio::test]
    async fn a_page_carries_its_own_rows_and_the_size_of_the_whole_library() {
        // The distinction the pager renders, and the one a page exists for: `items` is the window,
        // `total` is the library. Reporting the page's own length as `total` would make the Media
        // screen read "1-10 of 10" and hide the rest of the library with no error anywhere.
        let store = FakeMedia::default();
        stored(&store, 1, 25).await;

        let page = store
            .list_page(tenant(1), PageRequest::new(10, 0).expect("in range"))
            .await
            .expect("page");
        assert_eq!(page.items.len(), 10, "the window is the limit");
        assert_eq!(page.total, 25, "the total is the library, not the window");
    }

    #[tokio::test]
    async fn consecutive_pages_partition_the_library_without_overlap_or_gaps() {
        let store = FakeMedia::default();
        stored(&store, 1, 25).await;

        let mut seen = Vec::new();
        for offset in [0, 10, 20] {
            let page = store
                .list_page(tenant(1), PageRequest::new(10, offset).expect("in range"))
                .await
                .expect("page");
            assert_eq!(page.total, 25, "the total does not change as pages advance");
            seen.extend(page.items.into_iter().map(|row| row.media_id.to_string()));
        }
        assert_eq!(
            seen.len(),
            25,
            "three pages of ten cover twenty-five assets"
        );
        let mut unique = seen.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), 25, "no asset appears on two pages");
    }

    #[tokio::test]
    async fn a_page_past_the_end_is_empty_and_not_an_error() {
        // An operator on page four of a library that just had rows deleted gets an empty page.
        let store = FakeMedia::default();
        stored(&store, 1, 5).await;

        let page = store
            .list_page(tenant(1), PageRequest::new(10, 100).expect("in range"))
            .await
            .expect("a page past the end still reads");
        assert!(page.items.is_empty());
        assert_eq!(page.total, 5, "and still reports the library's size");
    }

    #[tokio::test]
    async fn the_paged_read_is_tenant_scoped_exactly_like_the_unpaged_one() {
        // The filter has to be identical on both reads: a page that leaked a neighbour's assets
        // would hand one tenant another's photographs.
        let store = FakeMedia::default();
        stored(&store, 1, 3).await;
        stored(&store, 2, 7).await;

        let page = store
            .list_page(tenant(1), PageRequest::new(100, 0).expect("in range"))
            .await
            .expect("page");
        assert_eq!(page.total, 3, "only this tenant's assets are counted");
        assert_eq!(page.items.len(), 3);
        assert_eq!(
            store.list(tenant(1)).await.expect("list").len(),
            3,
            "and the unpaged read agrees",
        );
    }
}
