// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The retention period, and the decision of whether a record is past it
//! ([ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md)).
//!
//! How long personal data may be kept is a legal decision, not a code default — it is a
//! configuration value, with a per-country default supplied by the country module
//! ([ADR-0027](../../../docs/adr/0027-country-modules.md)). So there is no baked-in period here:
//! [`RetentionPolicy`] is constructed from that configured value, and the cron does nothing until it
//! has one (masking on a guessed schedule would either erase data early or keep it too long, both
//! violations).

use core::time::Duration;

use pos_proto::time::Timestamp;

use super::subject::SubjectRecord;

/// How long personal data is retained before the cron masks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetentionPolicy {
    retain_for: Duration,
}

impl RetentionPolicy {
    /// A policy that retains personal data for `days` (from the configured/country value).
    #[must_use]
    pub const fn from_days(days: u32) -> Self {
        Self {
            retain_for: Duration::from_secs(days as u64 * 24 * 60 * 60),
        }
    }

    /// The retention window.
    #[must_use]
    pub const fn retain_for(&self) -> Duration {
        self.retain_for
    }

    /// The cutoff instant: a record collected at or before this, as of `now`, is past retention.
    #[must_use]
    pub fn cutoff(&self, now: Timestamp) -> Timestamp {
        let window_ms = i64::try_from(self.retain_for.as_millis()).unwrap_or(i64::MAX);
        let cutoff_ms = now.as_milliseconds_since_epoch().saturating_sub(window_ms);
        Timestamp::from_milliseconds_since_epoch(cutoff_ms)
            .unwrap_or_else(|_| Timestamp::from_milliseconds_since_epoch(0).unwrap_or(now))
    }

    /// Whether `record` is due for masking as of `now`: still holding personal data, and collected
    /// at or before the cutoff.
    #[must_use]
    pub fn is_due(&self, record: &SubjectRecord, now: Timestamp) -> bool {
        !record.is_masked() && record.collected_at <= self.cutoff(now)
    }
}

#[cfg(test)]
mod tests {
    use super::RetentionPolicy;

    use std::collections::BTreeMap;

    use pos_proto::ids::SubjectId;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;

    use crate::retention::subject::SubjectRecord;

    const DAY_MS: i64 = 24 * 60 * 60 * 1_000;

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(ms).expect("valid")
    }

    fn collected_at(ms: i64) -> SubjectRecord {
        SubjectRecord {
            subject_id: SubjectId::new(Ulid::from_u128(1)),
            collected_at: at(ms),
            fields: BTreeMap::new(),
            masked_at: None,
        }
    }

    #[test]
    fn a_record_older_than_the_window_is_due() {
        let policy = RetentionPolicy::from_days(30);
        let now = at(100 * DAY_MS);
        // Collected 31 days ago: past a 30-day window.
        assert!(policy.is_due(&collected_at(69 * DAY_MS), now));
    }

    #[test]
    fn a_record_within_the_window_is_not_due() {
        let policy = RetentionPolicy::from_days(30);
        let now = at(100 * DAY_MS);
        // Collected 10 days ago.
        assert!(!policy.is_due(&collected_at(90 * DAY_MS), now));
    }

    #[test]
    fn the_boundary_is_inclusive() {
        let policy = RetentionPolicy::from_days(30);
        let now = at(100 * DAY_MS);
        // Collected exactly 30 days ago is at the cutoff, and counts as past retention.
        assert!(policy.is_due(&collected_at(70 * DAY_MS), now));
    }

    #[test]
    fn an_already_masked_record_is_never_due() {
        let policy = RetentionPolicy::from_days(30);
        let now = at(100 * DAY_MS);
        let mut record = collected_at(0); // ancient
        record.masked_at = Some(at(1));
        assert!(
            !policy.is_due(&record, now),
            "a masked record is not re-masked"
        );
    }
}
