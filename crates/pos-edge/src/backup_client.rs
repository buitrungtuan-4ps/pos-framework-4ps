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
//!
//! # How often
//!
//! The store's `backup.interval_hours` setting, when its configuration sets one, and otherwise this
//! box's own interval: its file's deprecated `backup_interval_hours`, or a day
//! ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
//! decision 6). The loop reads it each time it schedules the next archive, so a change applies from
//! the next archive without a restart. No published value switches archiving off: a box whose file
//! sets `0` never starts this loop.

use core::future::Future;
use core::time::Duration;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use pos_ports::cloud_sync::CloudSync;
use pos_proto::backup::PublishedBackup;
use pos_proto::ids::StoreId;
use tokio::time::Instant;

use crate::app::Edge;
use crate::backup::{ArchiveError, ArchiveKey};
use crate::clock::SystemClock;
use crate::config::{ValueSource, log_in_force};
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

/// Where the loop reads the store's `backup` node from: the live configuration, which the
/// config-pull loop swaps while the store trades.
///
/// A seam rather than an `Arc<Edge<S>>`, so the loop carries no store type parameter — the same
/// shape as [`crate::auth::SessionSettingsSource`].
pub trait BackupSettingsSource: Send + Sync {
    /// The `backup` node the store is running now.
    fn backup_settings(&self) -> PublishedBackup;
}

impl<S: Send + Sync> BackupSettingsSource for Edge<S> {
    fn backup_settings(&self) -> PublishedBackup {
        self.session().backup
    }
}

/// Snapshots the store, seals it, and ships it — on a timer, until shutdown.
pub struct BackupClient<C> {
    cloud: C,
    store: StoreId,
    database: PathBuf,
    /// This box's own interval, which applies while the store's configuration sets none.
    interval: Duration,
    /// Where [`Self::interval`] comes from: the file's deprecated value, or the default.
    own: ValueSource,
    /// Where the store's published `backup` node is read from, when this follows one.
    settings: Option<Arc<dyn BackupSettingsSource>>,
}

impl<C: core::fmt::Debug> core::fmt::Debug for BackupClient<C> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BackupClient")
            .field("cloud", &self.cloud)
            .field("store", &self.store)
            .field("database", &self.database)
            .field("interval", &self.interval)
            .field("own", &self.own)
            .field("following", &self.settings.is_some())
            .finish()
    }
}

impl<C> BackupClient<C> {
    /// Builds a loop that archives the database at `database` for `store`, every `interval`.
    #[must_use]
    pub fn new(cloud: C, store: StoreId, database: PathBuf, interval: Duration) -> Self {
        Self {
            cloud,
            store,
            database,
            interval,
            own: ValueSource::Default,
            settings: None,
        }
    }

    /// Points the loop at the store's `backup` node, whose interval wins when it sets one
    /// ([ADR-0160](../../../docs/adr/0160-everything-a-store-runs-differently-is-published-configuration.md)
    /// decision 6). The interval given to [`Self::new`] applies while it sets none, and `own` says
    /// where that one comes from — this box's file or the default — for the log.
    #[must_use]
    pub fn following(mut self, settings: Arc<dyn BackupSettingsSource>, own: ValueSource) -> Self {
        self.settings = Some(settings);
        self.own = own;
        self
    }

    /// The interval in force now, and where it comes from: the store's `backup.interval_hours`
    /// when its configuration sets one within the bounds, else this box's own.
    #[must_use]
    pub fn interval_in_force(&self) -> (Duration, ValueSource) {
        self.settings
            .as_ref()
            .and_then(|settings| settings.backup_settings().interval_hours())
            .map_or((self.interval, self.own), |hours| {
                (
                    Duration::from_secs(u64::from(hours).saturating_mul(3600)),
                    ValueSource::Published,
                )
            })
    }

    /// Logs the interval in force and where it comes from (ADR-0160 decision 6).
    fn say_interval(&self, (interval, source): (Duration, ValueSource)) {
        log_in_force(
            "backup.interval_hours",
            "backup_interval_hours",
            interval.as_secs() / 3600,
            source,
            self.own == ValueSource::LocalFile,
        );
    }
}

impl<C> BackupClient<C>
where
    C: CloudSync,
{
    /// Archives on the timer until `shutdown` resolves.
    ///
    /// The first archive is [`SETTLE_BEFORE_FIRST_ARCHIVE`] after the loop starts, and each after it
    /// the interval in force when it is scheduled ([`next_due`]), so a change to the store's
    /// `backup.interval_hours` applies from the next archive. The interval is logged at start-up
    /// and again whenever it changes.
    ///
    /// A round in flight when the stop arrives is *not* abandoned: `select!` only races the wait, so
    /// the loop leaves after the archive it started finishes. That costs a stop the length of one
    /// upload and buys the store the backup it was in the middle of taking.
    pub async fn run(self, shutdown: impl Future<Output = ()> + Send) {
        let mut in_force = self.interval_in_force();
        self.say_interval(in_force);
        let mut due = Instant::now() + SETTLE_BEFORE_FIRST_ARCHIVE;
        let mut shutdown = core::pin::pin!(shutdown);
        loop {
            tokio::select! {
                () = &mut shutdown => break,
                () = tokio::time::sleep_until(due) => {
                    match self.archive_once().await {
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
                    }
                    // Read as the next archive is scheduled, so a published change applies from it.
                    let now_in_force = self.interval_in_force();
                    if now_in_force != in_force {
                        self.say_interval(now_in_force);
                        in_force = now_in_force;
                    }
                    due = next_due(due, Instant::now(), in_force.0);
                }
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

/// Longer than any box runs between restarts. An interval is held to it, so an absurd number in a
/// box's file schedules an archive that never comes rather than overflowing the clock.
const FAR_FUTURE: Duration = Duration::from_secs(30 * 365 * 24 * 60 * 60);

/// When the archive after the one due at `due` is due, once that one's round has ended at `ended`.
///
/// `interval` after `due`, so a store archives on a fixed timetable. A round that ran past that is
/// followed by the next archive at once, and the one after it `interval` later: a slow round costs
/// one archive taken late, never a burst of catch-up snapshots. That is the timetable this loop kept
/// with [`tokio::time::MissedTickBehavior::Delay`] before the interval could change while it ran.
fn next_due(due: Instant, ended: Instant, interval: Duration) -> Instant {
    due.checked_add(interval.min(FAR_FUTURE))
        .map_or(ended, |next| next.max(ended))
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
    use super::{BackupClient, BackupSettingsSource, next_due, snapshot_path};
    use crate::config::ValueSource;
    use core::time::Duration;
    use pos_fakes::FakeCloudSync;
    use pos_proto::backup::PublishedBackup;
    use pos_proto::ids::StoreId;
    use pos_proto::ulid::Ulid;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex};
    use tokio::time::Instant;

    const HOUR: Duration = Duration::from_secs(3600);

    /// A store's live `backup` node, which a test publishes to as the config-pull loop would.
    #[derive(Default)]
    struct LiveBackup(Mutex<PublishedBackup>);

    impl LiveBackup {
        fn publish(&self, interval_hours: Option<i64>) {
            *self.0.lock().expect("the node") = PublishedBackup { interval_hours };
        }
    }

    impl BackupSettingsSource for LiveBackup {
        fn backup_settings(&self) -> PublishedBackup {
            *self.0.lock().expect("the node")
        }
    }

    /// A loop over a box whose own interval is `own` from `source`, following `live`.
    fn following(
        live: &Arc<LiveBackup>,
        own: Duration,
        source: ValueSource,
    ) -> BackupClient<FakeCloudSync> {
        BackupClient::new(
            FakeCloudSync::new(),
            StoreId::new(Ulid::from_u128(0xB00)),
            PathBuf::from("store.sqlite"),
            own,
        )
        .following(Arc::<LiveBackup>::clone(live), source)
    }

    #[test]
    fn a_published_interval_schedules_the_next_archive_that_many_hours_on() {
        let live = Arc::new(LiveBackup::default());
        live.publish(Some(6));
        let client = following(&live, 24 * HOUR, ValueSource::Default);
        let (interval, source) = client.interval_in_force();
        assert_eq!((interval, source), (6 * HOUR, ValueSource::Published));

        let due = Instant::now();
        // A round of a few minutes, well inside the interval.
        let ended = due + Duration::from_secs(180);
        assert_eq!(next_due(due, ended, interval), due + 6 * HOUR);
    }

    #[test]
    fn with_nothing_published_the_box_keeps_its_own_interval() {
        let live = Arc::new(LiveBackup::default());
        // The file's deprecated value.
        let from_file = following(&live, 12 * HOUR, ValueSource::LocalFile);
        assert_eq!(
            from_file.interval_in_force(),
            (12 * HOUR, ValueSource::LocalFile)
        );
        // Neither: a day.
        let neither = following(&live, 24 * HOUR, ValueSource::Default);
        assert_eq!(
            neither.interval_in_force(),
            (24 * HOUR, ValueSource::Default)
        );
        // A loop that follows no node at all, as before the setting.
        let unfollowed = BackupClient::new(
            FakeCloudSync::new(),
            StoreId::new(Ulid::from_u128(0xB00)),
            PathBuf::from("store.sqlite"),
            24 * HOUR,
        );
        assert_eq!(
            unfollowed.interval_in_force(),
            (24 * HOUR, ValueSource::Default)
        );
    }

    #[test]
    fn a_published_interval_out_of_bounds_falls_back_to_the_boxs_own() {
        let live = Arc::new(LiveBackup::default());
        let client = following(&live, 12 * HOUR, ValueSource::LocalFile);
        // No published value switches archiving off, and none past a week is read.
        for outside in [0, -1, 169] {
            live.publish(Some(outside));
            assert_eq!(
                client.interval_in_force(),
                (12 * HOUR, ValueSource::LocalFile),
                "{outside}"
            );
        }
    }

    #[test]
    fn a_change_applies_from_the_next_archive() {
        // The loop reads the interval as it schedules each archive: a value published between two
        // archives is the one the next is scheduled with, without a restart.
        let live = Arc::new(LiveBackup::default());
        let client = following(&live, 24 * HOUR, ValueSource::Default);
        let first = Instant::now();
        let second = next_due(first, first, client.interval_in_force().0);
        assert_eq!(second, first + 24 * HOUR);

        live.publish(Some(6));
        let third = next_due(second, second, client.interval_in_force().0);
        assert_eq!(
            third,
            second + 6 * HOUR,
            "the new interval, from the next archive"
        );

        live.publish(None);
        let fourth = next_due(third, third, client.interval_in_force().0);
        assert_eq!(
            fourth,
            third + 24 * HOUR,
            "and the box's own again once it is cleared"
        );
    }

    #[test]
    fn a_round_that_runs_past_its_interval_is_followed_at_once_and_then_on_time() {
        let due = Instant::now();
        // A round that took two hours against a one-hour interval: the next archive at once, as
        // the old timer's delayed tick did, rather than a burst of the archives it missed.
        let ended = due + 2 * HOUR;
        let late = next_due(due, ended, HOUR);
        assert_eq!(late, ended);
        // And the one after it an interval after that one began.
        assert_eq!(
            next_due(late, late + Duration::from_secs(60), HOUR),
            late + HOUR
        );
    }

    #[test]
    fn an_interval_past_what_the_clock_counts_never_comes_rather_than_overflowing() {
        let due = Instant::now();
        let never = next_due(due, due, Duration::MAX);
        assert!(
            never > due + 24 * 365 * HOUR,
            "an archive no box lives to see"
        );
    }

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
