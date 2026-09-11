// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The sweep that ages a store's archives out
//! ([ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md), slice 3).
//!
//! # Why an archive expires, which is not about disk
//!
//! Disk is the cheap reason. The real one is that a store's archive is its **whole database**, and
//! a store database carries the buyer name and tax code a B2B invoice needs
//! ([ADR-0107](../../../docs/adr/0107-the-buyer-is-a-subject.md)) — T1 data. The subject-masking
//! cron ([`crate::retention`], [ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md))
//! redacts that data in the *live* store on a legal retention period. An archive taken before the
//! masking preserves exactly what the masking removed.
//!
//! Capping the archive window strictly below the subject window is what makes the redaction
//! eventually true everywhere rather than true only where somebody looked: past the archive
//! window, the last copy still holding a buyer's details is gone, and nobody had to remember to go
//! and get it. [`crate::config`] refuses to start when the two windows are the wrong way round, so
//! this is a property of the running system rather than a paragraph in a runbook.
//!
//! It is deliberately **not** an erasure mechanism. A specific person asking to be erased is a
//! deliberate act escalated to the Data Protection contact, and
//! [`docs/guides/data-subject-requests.md`](../../../docs/guides/data-subject-requests.md)
//! carries the procedure for the case where an erasure must reach an archive before the window
//! does. This sweep is the automatic, time-based half — the same split ADR-0035 already drew.
//!
//! # Bytes first, row second
//!
//! One round removes the sealed object and then forgets the row that names it. The order is the
//! decision: a crash between the two leaves a row pointing at nothing, which the next round finds
//! and retries (`BlobStore::delete` succeeds whether or not the object was there). Row-first would
//! leave the object with nothing naming it — an unreachable thing in a bucket, costing money
//! forever, that only a manual listing would ever find again.

use core::future::Future;
use core::time::Duration;

use pos_ports::{BlobKey, BlobStore};
use pos_proto::determinism::ClockSource;
use pos_proto::time::Timestamp;

use crate::archive::{ArchiveStore, ArchiveStoreError};

/// How many archives one round removes before starting a fresh page.
///
/// Small, because each one is a network round trip to the object store rather than a row: a bigger
/// page would hold a database connection open across hundreds of deletes for no gain.
const BATCH: i64 = 100;

/// How often the sweep runs when nothing says otherwise — daily, like the subject cron it sits
/// beside, and ample for a window measured in weeks.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// How long an archive is kept. Days, because that is the unit an operator reasons in and the unit
/// the subject window it must stay under is expressed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveWindow {
    days: u32,
}

impl ArchiveWindow {
    /// A window of `days`.
    #[must_use]
    pub const fn days(days: u32) -> Self {
        Self { days }
    }

    /// The instant at or before which an archive is past this window, as of `now`.
    ///
    /// Saturating rather than wrapping: a clock far enough behind the epoch to underflow yields
    /// [`Timestamp::EPOCH`], which expires nothing. Expiring everything would be the other
    /// direction, and a clock fault must not delete a fleet's backups.
    #[must_use]
    pub fn cutoff(self, now: Timestamp) -> i64 {
        let window_ms = i64::from(self.days).saturating_mul(24 * 60 * 60 * 1000);
        now.as_milliseconds_since_epoch()
            .saturating_sub(window_ms)
            .max(Timestamp::EPOCH.as_milliseconds_since_epoch())
    }
}

/// What one sweep achieved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// How many archives were removed, bytes and row together.
    pub removed: u64,
    /// How many the sweep could not remove this round and left for the next one.
    pub left: u64,
}

/// Removes every archive past `window` as of `now`, in bounded pages.
///
/// An archive whose bytes will not delete is **left whole** — the row stays, so the next round
/// tries again and the console keeps telling the truth about what exists. Counting it and moving
/// on is what stops one unreachable object from stalling the sweep for every other store.
///
/// # Errors
///
/// [`ArchiveStoreError`] if the registry itself could not be read or written; a partial sweep has
/// still removed whatever it committed, and the next run resumes.
pub async fn sweep<A, B>(
    archives: &A,
    blobs: &B,
    window: ArchiveWindow,
    now: Timestamp,
) -> Result<SweepReport, ArchiveStoreError>
where
    A: ArchiveStore,
    B: BlobStore,
{
    let cutoff = window.cutoff(now);
    let mut report = SweepReport::default();
    loop {
        let expired = archives.expired_before(cutoff, BATCH).await?;
        if expired.is_empty() {
            break;
        }
        let page = i64::try_from(expired.len()).unwrap_or(BATCH);
        let mut removed_this_page = 0_u64;
        for archive in expired {
            let Ok(key) = BlobKey::parse(&archive.object_key) else {
                // A key the object store will not accept cannot be deleted through it, and the row
                // would otherwise be immortal. Forgetting the row is the honest outcome: the object
                // was never reachable through this path, and an operator with a bucket listing is
                // the only one who can now find it.
                tracing::warn!(
                    key = archive.object_key.as_str(),
                    "an expired archive names an unusable object key; forgetting the row"
                );
                archives
                    .forget_archive(archive.tenant, archive.store_id, archive.taken_at)
                    .await?;
                removed_this_page = removed_this_page.saturating_add(1);
                continue;
            };
            if let Err(error) = blobs.delete(&key).await {
                tracing::warn!(
                    %error,
                    key = archive.object_key.as_str(),
                    "an expired archive's bytes did not delete; leaving the row for the next sweep"
                );
                report.left = report.left.saturating_add(1);
                continue;
            }
            archives
                .forget_archive(archive.tenant, archive.store_id, archive.taken_at)
                .await?;
            removed_this_page = removed_this_page.saturating_add(1);
        }
        report.removed = report.removed.saturating_add(removed_this_page);
        // A page that removed nothing is a page of rows the object store refuses; another identical
        // page would follow it forever, so the round ends and the next interval retries.
        if page < BATCH || removed_this_page == 0 {
            break;
        }
    }
    Ok(report)
}

/// Runs the archive sweep on `interval` until `shutdown` resolves, taking `now` from `clock`.
///
/// A failed sweep is logged and retried next tick rather than crashing the cloud — a retention
/// pass a day late is a far smaller problem than a cloud that will not start, and the window is
/// measured in weeks.
pub async fn run<A, B, C, H>(
    archives: A,
    blobs: B,
    window: ArchiveWindow,
    clock: C,
    health: H,
    interval: Duration,
    shutdown: impl Future<Output = ()>,
) where
    A: ArchiveStore,
    B: BlobStore,
    C: ClockSource,
    H: crate::health::TaskHealthStore,
{
    tokio::pin!(shutdown);
    loop {
        let outcome = sweep(&archives, &blobs, window, clock.now()).await;
        let detail = match &outcome {
            Ok(report) => {
                if report.removed > 0 || report.left > 0 {
                    tracing::info!(
                        removed = report.removed,
                        left = report.left,
                        "store-archive retention sweep ran"
                    );
                } else {
                    tracing::debug!("store-archive retention sweep found nothing past the window");
                }
                crate::health::tick_detail(
                    report.left == 0,
                    interval.as_secs(),
                    serde_json::json!({ "removed": report.removed, "left": report.left }),
                )
            }
            Err(error) => {
                tracing::error!(
                    %error,
                    "store-archive retention sweep failed; will retry next interval"
                );
                crate::health::tick_detail(false, interval.as_secs(), serde_json::json!({}))
            }
        };
        // Best-effort telemetry: failing to record health must never stop the sweep.
        if let Err(error) = health
            .record_tick(crate::health::ARCHIVE_RETENTION, clock.now(), &detail)
            .await
        {
            tracing::warn!(%error, "recording archive-retention task health failed");
        }
        tokio::select! {
            biased;
            () = &mut shutdown => {
                tracing::info!("store-archive retention sweep shutting down");
                return;
            }
            () = tokio::time::sleep(interval) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use pos_ports::{BlobKey, BlobStore, PortError, PortName};
    use pos_proto::ids::{StoreId, TenantId};
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;

    use super::{ArchiveWindow, sweep};
    use crate::archive::{
        ArchiveStore, ArchiveStoreError, ExpiredArchive, StoreArchive, archive_object_key,
    };

    fn tenant() -> TenantId {
        TenantId::new(Ulid::from_u128(1))
    }

    fn store(n: u128) -> StoreId {
        StoreId::new(Ulid::from_u128(n))
    }

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(ms).unwrap_or(Timestamp::EPOCH)
    }

    /// A registry holding whatever the test put in it.
    #[derive(Debug, Default)]
    struct Registry {
        rows: Mutex<Vec<ExpiredArchive>>,
    }

    impl Registry {
        fn holding(entries: &[(StoreId, i64)]) -> Self {
            Self {
                rows: Mutex::new(
                    entries
                        .iter()
                        .map(|&(store_id, taken_at)| ExpiredArchive {
                            tenant: tenant(),
                            store_id,
                            taken_at,
                            object_key: archive_object_key(store_id, taken_at),
                        })
                        .collect(),
                ),
            }
        }

        fn remaining(&self) -> Vec<i64> {
            self.rows
                .lock()
                .expect("the rows are not poisoned")
                .iter()
                .map(|row| row.taken_at)
                .collect()
        }
    }

    impl ArchiveStore for Registry {
        async fn wrapped_key(
            &self,
            _tenant: TenantId,
            _store: StoreId,
        ) -> Result<Option<String>, ArchiveStoreError> {
            Ok(None)
        }

        async fn adopt_key(
            &self,
            _tenant: TenantId,
            _store: StoreId,
            wrapped: &str,
            _minted_at: i64,
        ) -> Result<String, ArchiveStoreError> {
            Ok(wrapped.to_owned())
        }

        async fn record_archive(
            &self,
            _tenant: TenantId,
            _archive: &StoreArchive,
        ) -> Result<(), ArchiveStoreError> {
            Ok(())
        }

        async fn list_archives(
            &self,
            _tenant: TenantId,
            _store: StoreId,
            _limit: i64,
        ) -> Result<Vec<StoreArchive>, ArchiveStoreError> {
            Ok(Vec::new())
        }

        async fn expired_before(
            &self,
            cutoff: i64,
            limit: i64,
        ) -> Result<Vec<ExpiredArchive>, ArchiveStoreError> {
            let rows = self.rows.lock().expect("the rows are not poisoned");
            Ok(rows
                .iter()
                .filter(|row| row.taken_at <= cutoff)
                .take(usize::try_from(limit).unwrap_or(usize::MAX))
                .cloned()
                .collect())
        }

        async fn forget_archive(
            &self,
            _tenant: TenantId,
            store: StoreId,
            taken_at: i64,
        ) -> Result<(), ArchiveStoreError> {
            self.rows
                .lock()
                .expect("the rows are not poisoned")
                .retain(|row| !(row.store_id == store && row.taken_at == taken_at));
            Ok(())
        }
    }

    /// An object store that can be told to refuse one key.
    #[derive(Debug, Default)]
    struct Bucket {
        objects: Mutex<BTreeMap<String, Vec<u8>>>,
        refuses: Option<String>,
    }

    impl BlobStore for Bucket {
        async fn put(&self, key: &BlobKey, body: &[u8]) -> Result<(), PortError> {
            self.objects
                .lock()
                .expect("the bucket is not poisoned")
                .insert(key.as_str().to_owned(), body.to_vec());
            Ok(())
        }

        async fn get(&self, key: &BlobKey) -> Result<Option<Vec<u8>>, PortError> {
            Ok(self
                .objects
                .lock()
                .expect("the bucket is not poisoned")
                .get(key.as_str())
                .cloned())
        }

        async fn delete(&self, key: &BlobKey) -> Result<(), PortError> {
            if self.refuses.as_deref() == Some(key.as_str()) {
                return Err(PortError::unavailable(PortName::BlobStore, "no"));
            }
            self.objects
                .lock()
                .expect("the bucket is not poisoned")
                .remove(key.as_str());
            Ok(())
        }

        async fn list(&self, prefix: &BlobKey) -> Result<Vec<BlobKey>, PortError> {
            Ok(self
                .objects
                .lock()
                .expect("the bucket is not poisoned")
                .keys()
                .filter(|key| key.starts_with(prefix.as_str()))
                .filter_map(|key| BlobKey::parse(key).ok())
                .collect())
        }
    }

    const DAY: i64 = 24 * 60 * 60 * 1000;

    #[tokio::test]
    async fn an_archive_past_the_window_goes_and_a_recent_one_stays() {
        let now = at(100 * DAY);
        let old = 60 * DAY;
        let recent = 95 * DAY;
        let registry = Registry::holding(&[(store(7), old), (store(7), recent)]);
        let bucket = Bucket::default();
        for taken_at in [old, recent] {
            let key = BlobKey::parse(&archive_object_key(store(7), taken_at)).expect("a valid key");
            bucket.put(&key, b"sealed").await.expect("seed the bucket");
        }

        let report = sweep(&registry, &bucket, ArchiveWindow::days(30), now)
            .await
            .expect("the sweep runs");

        assert_eq!(report.removed, 1, "only the one past the window");
        assert_eq!(registry.remaining(), vec![recent]);
        let gone = BlobKey::parse(&archive_object_key(store(7), old)).expect("a valid key");
        assert!(
            bucket.get(&gone).await.expect("read").is_none(),
            "the sealed bytes went, not only the row"
        );
    }

    /// The order that matters: if the bytes will not go, the row stays, so the next sweep tries
    /// again. The other order would leave an object nothing names.
    #[tokio::test]
    async fn bytes_that_will_not_delete_leave_their_row_behind() {
        let now = at(100 * DAY);
        let stuck = 10 * DAY;
        let fine = 11 * DAY;
        let registry = Registry::holding(&[(store(7), stuck), (store(8), fine)]);
        let bucket = Bucket {
            objects: Mutex::default(),
            refuses: Some(archive_object_key(store(7), stuck)),
        };

        let report = sweep(&registry, &bucket, ArchiveWindow::days(30), now)
            .await
            .expect("the sweep runs");

        assert_eq!(report.removed, 1, "the one that could go, went");
        assert_eq!(report.left, 1, "and the one that could not is counted");
        assert_eq!(
            registry.remaining(),
            vec![stuck],
            "the row for the undeleted object survives, so the next sweep retries it"
        );
    }

    /// A window nothing has passed yet removes nothing and does not loop.
    #[tokio::test]
    async fn a_fleet_inside_its_window_is_left_alone() {
        let now = at(10 * DAY);
        let registry = Registry::holding(&[(store(7), 9 * DAY), (store(8), 8 * DAY)]);
        let report = sweep(&registry, &Bucket::default(), ArchiveWindow::days(30), now)
            .await
            .expect("the sweep runs");
        assert_eq!(report, super::SweepReport::default());
        assert_eq!(registry.remaining().len(), 2);
    }

    /// A clock behind the epoch expires nothing rather than everything: a fleet's backups must not
    /// be deleted by a bad NTP reply.
    #[test]
    fn a_clock_before_the_epoch_expires_nothing() {
        assert_eq!(
            ArchiveWindow::days(30).cutoff(Timestamp::EPOCH),
            Timestamp::EPOCH.as_milliseconds_since_epoch(),
            "the cutoff never runs backwards past the epoch"
        );
    }
}
