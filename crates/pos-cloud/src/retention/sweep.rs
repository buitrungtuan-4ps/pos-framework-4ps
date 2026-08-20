// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The retention sweep and its daily runner
//! ([ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md)).
//!
//! [`sweep`] reads the subject store's records that are past retention, masks each ([`SubjectRecord`]
//! masking), and writes them back — in bounded pages, so a large backlog does not load the whole
//! table into memory. It is **idempotent**: the store returns only unmasked records, so a masked one
//! is never revisited, and re-running the sweep after a crash simply resumes. [`run`] repeats it on a
//! schedule until shutdown.
//!
//! This enforces the *time-based* retention policy automatically. It is **not** the path for an
//! individual's erasure/access/portability request — those are escalated to the Data Protection
//! contact and actioned deliberately, never on a cron (ADR-0035).

use core::future::Future;
use core::time::Duration;

use pos_proto::determinism::ClockSource;
use pos_proto::time::Timestamp;

use super::policy::RetentionPolicy;
use super::subject::SubjectRecord;

/// How many records one page of the sweep reads and masks.
const BATCH: u32 = 500;

/// How often [`run`] sweeps, by default — daily is ample for a period measured in months.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// The subject store the retention cron masks over.
///
/// The seam between the retention engine and PostgreSQL, so the engine is tested without a database.
/// The implementation's contract is what makes the sweep terminate and stay idempotent:
/// [`due_before`](SubjectStore::due_before) must return only records whose `masked_at` is unset, so a
/// record masked by [`save_masked`](SubjectStore::save_masked) is never handed back.
pub trait SubjectStore {
    /// The unmasked records collected at or before `cutoff`, at most `limit` of them.
    fn due_before(
        &self,
        cutoff: Timestamp,
        limit: u32,
    ) -> impl Future<Output = Result<Vec<SubjectRecord>, RetentionError>> + Send;

    /// Persists masked records, returning how many rows were updated.
    fn save_masked(
        &self,
        records: &[SubjectRecord],
    ) -> impl Future<Output = Result<u64, RetentionError>> + Send;
}

/// A subject-store operation failed.
#[derive(Debug, Clone, thiserror::Error)]
#[error("subject store error: {message}")]
pub struct RetentionError {
    message: String,
}

impl RetentionError {
    /// Builds an error with a human-readable reason.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// What one sweep achieved.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SweepReport {
    /// How many records were masked.
    pub masked: u64,
    /// How many pages were processed.
    pub batches: u32,
}

/// Masks every record past retention as of `now`, in bounded pages.
///
/// # Errors
///
/// [`RetentionError`] if the store cannot be read or written; a partial sweep has still masked
/// whatever it committed, and the next run resumes (masking is idempotent).
pub async fn sweep<S>(
    store: &S,
    policy: RetentionPolicy,
    now: Timestamp,
) -> Result<SweepReport, RetentionError>
where
    S: SubjectStore,
{
    let cutoff = policy.cutoff(now);
    let mut report = SweepReport::default();
    loop {
        let due = store.due_before(cutoff, BATCH).await?;
        if due.is_empty() {
            break;
        }
        let page = u32::try_from(due.len()).unwrap_or(BATCH);
        let masked: Vec<SubjectRecord> = due.iter().map(|record| record.masked(now)).collect();
        let saved = store.save_masked(&masked).await?;
        report.masked = report.masked.saturating_add(saved);
        report.batches = report.batches.saturating_add(1);
        if page < BATCH {
            break;
        }
    }
    Ok(report)
}

/// Runs the retention sweep on `interval` until `shutdown` resolves, taking `now` from `clock`.
///
/// A sweep error is logged and retried on the next tick rather than crashing the cloud — a masking
/// pass that is a day late is a far smaller problem than a cloud that will not start.
pub async fn run<S, C>(
    store: S,
    policy: RetentionPolicy,
    clock: C,
    interval: Duration,
    shutdown: impl Future<Output = ()>,
) where
    S: SubjectStore,
    C: ClockSource,
{
    tokio::pin!(shutdown);
    loop {
        match sweep(&store, policy, clock.now()).await {
            Ok(report) if report.masked > 0 => {
                tracing::info!(
                    masked = report.masked,
                    batches = report.batches,
                    "retention sweep masked records past their retention period"
                );
            }
            Ok(_) => tracing::debug!("retention sweep found nothing past retention"),
            Err(error) => {
                tracing::error!(error = %error, "retention sweep failed; will retry next interval");
            }
        }
        tokio::select! {
            biased;
            () = &mut shutdown => {
                tracing::info!("retention cron shutting down");
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

    use super::{RetentionError, SubjectStore, sweep};

    use pos_proto::ids::SubjectId;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;

    use crate::retention::policy::RetentionPolicy;
    use crate::retention::subject::{REDACTION, SubjectRecord};

    const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(ms).expect("valid")
    }

    /// A record collected at `collected_ms`, with synthetic placeholder fields.
    fn record(id: u128, collected_ms: i64) -> SubjectRecord {
        let mut fields = BTreeMap::new();
        fields.insert("name".to_owned(), format!("name-placeholder-{id}"));
        fields.insert("phone".to_owned(), format!("phone-placeholder-{id}"));
        SubjectRecord {
            subject_id: SubjectId::new(Ulid::from_u128(id)),
            collected_at: at(collected_ms),
            fields,
            masked_at: None,
        }
    }

    /// An in-memory subject store honouring the "only unmasked before cutoff" contract.
    struct FakeStore {
        rows: Mutex<Vec<SubjectRecord>>,
    }

    impl FakeStore {
        fn with(rows: Vec<SubjectRecord>) -> Self {
            Self {
                rows: Mutex::new(rows),
            }
        }

        fn snapshot(&self) -> Vec<SubjectRecord> {
            self.rows.lock().expect("lock").clone()
        }
    }

    impl SubjectStore for FakeStore {
        async fn due_before(
            &self,
            cutoff: Timestamp,
            limit: u32,
        ) -> Result<Vec<SubjectRecord>, RetentionError> {
            let rows = self.rows.lock().expect("lock");
            Ok(rows
                .iter()
                .filter(|row| row.masked_at.is_none() && row.collected_at <= cutoff)
                .take(limit as usize)
                .cloned()
                .collect())
        }

        async fn save_masked(&self, masked: &[SubjectRecord]) -> Result<u64, RetentionError> {
            let mut rows = self.rows.lock().expect("lock");
            let mut saved = 0;
            for update in masked {
                if let Some(row) = rows
                    .iter_mut()
                    .find(|row| row.subject_id == update.subject_id)
                {
                    *row = update.clone();
                    saved += 1;
                }
            }
            Ok(saved)
        }
    }

    #[tokio::test]
    async fn it_masks_records_past_retention_and_leaves_the_rest() {
        let policy = RetentionPolicy::from_days(30);
        let now = at(100 * DAY_MS);
        let store = FakeStore::with(vec![
            record(1, 10 * DAY_MS), // 90 days old: due
            record(2, 95 * DAY_MS), // 5 days old: within retention
            record(3, 40 * DAY_MS), // 60 days old: due
        ]);

        let report = sweep(&store, policy, now).await.expect("sweep");
        assert_eq!(report.masked, 2);

        let rows = store.snapshot();
        let masked: Vec<_> = rows
            .iter()
            .filter(|row| row.is_masked())
            .map(|row| row.subject_id)
            .collect();
        assert_eq!(masked.len(), 2, "the two old records were masked");
        for row in &rows {
            if row.is_masked() {
                assert!(row.fields.values().all(|value| value == REDACTION));
            } else {
                // The in-retention record keeps its (placeholder) data untouched.
                assert!(row.fields.values().all(|value| value != REDACTION));
            }
        }
    }

    #[tokio::test]
    async fn a_second_sweep_masks_nothing() {
        let policy = RetentionPolicy::from_days(30);
        let now = at(100 * DAY_MS);
        let store = FakeStore::with(vec![record(1, 10 * DAY_MS), record(2, 20 * DAY_MS)]);

        let first = sweep(&store, policy, now).await.expect("first sweep");
        assert_eq!(first.masked, 2);
        let second = sweep(&store, policy, now).await.expect("second sweep");
        assert_eq!(second.masked, 0, "already-masked records are not revisited");
    }

    #[tokio::test]
    async fn masking_preserves_the_subject_id_so_reconciliation_survives() {
        let policy = RetentionPolicy::from_days(1);
        let now = at(100 * DAY_MS);
        let original = record(0xABCD, 0);
        let store = FakeStore::with(vec![original.clone()]);

        sweep(&store, policy, now).await.expect("sweep");
        let rows = store.snapshot();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].subject_id, original.subject_id,
            "the id an invoice references survives masking"
        );
        assert!(rows[0].is_masked());
    }
}
