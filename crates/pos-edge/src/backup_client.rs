// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The loop that actually backs a store up
//! ([ADR-0124](../../../docs/adr/0124-a-store-that-can-be-restored.md)).
//!
//! Everything else in D-2 is machinery this loop uses: [`store_sqlite::snapshot_to`] takes a
//! consistent copy of a live WAL database, [`crate::backup`] seals that copy under a key only this
//! store and the operator hold, and [`CloudSync::upload_archive`] ships the ciphertext. What is
//! decided *here* is when, how often, and what a failure means.
//!
//! # Three properties worth stating, because each is a decision
//!
//! **The seal happens at the till.** The key is fetched, the archive is built and encrypted on this
//! box, and only ciphertext leaves it. A store's `subjects` table (ADR-0107) is the only T1 data a
//! till holds and it is published nowhere else, so shipping the database is the first time buyer
//! details would leave the shop — sealing before they do is what keeps the relay, the object store
//! and the off-box tier holding bytes none of them can read.
//!
//! **A superseded box keeps archiving.** This loop never consults the lease
//! ([ADR-0123](../../../docs/adr/0123-a-superseded-box-opens-nothing-new.md)), and that is
//! deliberate rather than an omission: a box that has been replaced holds exactly the events its
//! replacement does not — the ones committed after the cut-over began and never published. Stopping
//! its backups is stopping the backups of the only copy.
//!
//! **A failed round is a warning, never a stop.** Nothing here can keep the shop from trading. The
//! cloud may be unreachable, the disk may be too full for a snapshot, the archive may exceed the
//! cap: each is logged and retried on the next tick, and the till never learns any of it happened.

use core::future::Future;
use core::time::Duration;
use std::path::{Path, PathBuf};

use pos_ports::cloud_sync::CloudSync;
use pos_proto::ids::StoreId;

use crate::backup::{ArchiveError, ArchiveKey};
use crate::clock::SystemClock;
use pos_ports::ClockSource;

/// How long a box runs before its first archive.
///
/// Not zero, and not the whole interval either. Archiving the instant a process starts would put a
/// snapshot in the middle of every boot — including the boot that follows an OTA install, and every
/// boot of a box in a crash loop. Waiting a full interval would mean a box restarted daily, which is
/// an ordinary shop, never archives at all. Five minutes clears the start-up and loses nothing.
const SETTLE_BEFORE_FIRST_ARCHIVE: Duration = Duration::from_secs(300);

/// Why a round of archiving did not finish.
///
/// Every variant is retried on the next tick. They are kept apart because they read very differently
/// in a log: a cloud that will not issue a key is an operator's problem with the fleet, a snapshot
/// that will not write is an operator's problem with this box's disk, and a key that will not parse
/// is a bug in one of the two.
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// The cloud would not issue this store's archive key, or would not accept the archive.
    #[error("the cloud could not be reached for a store archive: {0}")]
    Cloud(pos_ports::PortError),
    /// The cloud answered with something that is not an archive key.
    #[error("the cloud issued an unusable archive key: {0}")]
    Key(ArchiveError),
    /// A consistent copy of the live database could not be written.
    #[error("the store could not be snapshotted: {0}")]
    Snapshot(pos_ports::PortError),
    /// The snapshot could not be sealed.
    #[error("the store snapshot could not be sealed: {0}")]
    Archive(ArchiveError),
    /// The blocking snapshot task did not finish — the runtime is shutting down.
    #[error("the archive task did not finish: {0}")]
    Task(#[from] tokio::task::JoinError),
}

/// Snapshots the store, seals it, and ships it — on a timer, until shutdown.
#[derive(Debug)]
pub struct BackupClient<C> {
    cloud: C,
    store: StoreId,
    database: PathBuf,
    interval: Duration,
}

impl<C> BackupClient<C>
where
    C: CloudSync,
{
    /// Builds a loop that archives the database at `database` for `store`, every `interval`.
    #[must_use]
    pub fn new(cloud: C, store: StoreId, database: PathBuf, interval: Duration) -> Self {
        Self {
            cloud,
            store,
            database,
            interval,
        }
    }

    /// Archives on the timer until `shutdown` resolves.
    ///
    /// A round in flight when the stop arrives is *not* abandoned: `select!` only races the tick, so
    /// the loop leaves after the archive it started finishes. That costs a stop the length of one
    /// upload and buys the store the backup it was in the middle of taking.
    pub async fn run(self, shutdown: impl Future<Output = ()> + Send) {
        let mut ticks = tokio::time::interval_at(
            tokio::time::Instant::now() + SETTLE_BEFORE_FIRST_ARCHIVE,
            self.interval,
        );
        // A round that overran its interval must not be followed by a burst of catch-up snapshots:
        // the next one is simply due an interval after this one ended.
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut shutdown = core::pin::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                _instant = ticks.tick() => match self.archive_once().await {
                    Ok(bytes) => tracing::info!(
                        store = %self.store,
                        bytes,
                        "shipped a sealed store archive"
                    ),
                    Err(error) => tracing::warn!(
                        %error,
                        store = %self.store,
                        "this store was not archived this round; trying again next interval"
                    ),
                },
            }
        }
        tracing::debug!("the store-archive loop stopped");
    }

    /// One round: fetch the key, snapshot, seal, upload. Returns the sealed size in bytes.
    ///
    /// Public because "archive this store now" is a real operation and not only a tick of
    /// [`Self::run`] — it is what a test drives, and what an operator-triggered archive would call.
    /// It is safe to call while the loop is running: the two would contend for the working copy,
    /// but the archive that lost the race fails its round and retries, and neither can corrupt the
    /// live database, which is only ever read.
    ///
    /// # Errors
    ///
    /// [`BackupError`] naming which of the four steps did not complete.
    pub async fn archive_once(&self) -> Result<usize, BackupError> {
        let issued = self
            .cloud
            .archive_key(self.store)
            .await
            .map_err(BackupError::Cloud)?;
        let key = ArchiveKey::parse(&issued).map_err(BackupError::Key)?;
        // Stamped before the snapshot, because the recovery point an operator reads is when the
        // copy was taken — not when a slow upload of it happened to land.
        let taken_at = SystemClock.now();
        let database = self.database.clone();
        let store = self.store;
        // SQLite's API is synchronous and a snapshot of a busy store is seconds of work, so it runs
        // where blocking is allowed rather than stalling this runtime's other tasks.
        let sealed =
            tokio::task::spawn_blocking(move || seal_snapshot(&database, &key, store)).await??;
        let size = sealed.len();
        self.cloud
            .upload_archive(self.store, taken_at, &sealed)
            .await
            .map_err(BackupError::Cloud)?;
        Ok(size)
    }
}

/// Where a round writes its working copy: beside the database, never in the system temp directory.
///
/// `VACUUM INTO` writes a whole second database, so the destination has to be somewhere with room
/// for one — and beside the live file is the one place an operator has already sized for that. A
/// system temp directory is frequently a small tmpfs, where the snapshot of a real store fails.
fn snapshot_path(database: &Path) -> PathBuf {
    let mut path = database.as_os_str().to_os_string();
    path.push(".snapshot");
    PathBuf::from(path)
}

/// The blocking half of a round: a consistent copy, sealed, with the copy removed either way.
///
/// The plaintext snapshot is the one artefact here that is worth worrying about — it is the whole
/// store, buyer details included, sitting unencrypted on disk. It exists for as long as the seal
/// takes and is removed on both paths, including the failing one.
fn seal_snapshot(
    database: &Path,
    key: &ArchiveKey,
    store: StoreId,
) -> Result<Vec<u8>, BackupError> {
    let snapshot = snapshot_path(database);
    // A round killed mid-flight leaves this behind, and SQLite refuses to vacuum into a file that
    // already exists — so a leftover would make every subsequent round fail until somebody noticed.
    if snapshot.exists() {
        std::fs::remove_file(&snapshot).map_err(|error| {
            BackupError::Snapshot(
                pos_ports::PortError::unavailable(
                    pos_ports::PortName::EventStore,
                    "the previous round's working copy could not be removed",
                )
                .with_source(error),
            )
        })?;
    }
    store_sqlite::snapshot_to(database, &snapshot).map_err(BackupError::Snapshot)?;
    let sealed = crate::backup::seal_file(&snapshot, key, store);
    // Removed before the result is inspected: a seal that failed leaves the same plaintext behind
    // as one that succeeded, and it is the failing path nobody re-reads.
    let _ignored = std::fs::remove_file(&snapshot);
    sealed.map_err(BackupError::Archive)
}

#[cfg(test)]
mod tests {
    use super::snapshot_path;
    use std::path::{Path, PathBuf};

    #[test]
    fn the_working_copy_sits_beside_the_database() {
        let path = snapshot_path(Path::new("/var/lib/pos-edge/store.sqlite"));
        assert_eq!(
            path,
            PathBuf::from("/var/lib/pos-edge/store.sqlite.snapshot"),
            "the snapshot goes where the database already is, not in a system temp directory"
        );
    }

    #[test]
    fn a_bare_filename_keeps_its_directory() {
        assert_eq!(
            snapshot_path(Path::new("store.sqlite")),
            PathBuf::from("store.sqlite.snapshot"),
            "a relative store path stays relative to the service unit's working directory"
        );
    }
}
