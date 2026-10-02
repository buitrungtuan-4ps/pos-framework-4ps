// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The cloud's record of the one-time data changes it makes through its own code.
//!
//! A one-time change written in SQL claims its marker in `data_migrations` in the statement that
//! makes the change (migrations 0073, 0074 and 0076). A change the cloud makes through its own code,
//! such as publishing a configuration version to every store ([`crate::qr_ordering`]), cannot share
//! a statement with its marker. So it asks [`DataMigrations::recorded_time`] first and calls
//! [`DataMigrations::record`] once the change is done. Such a change must also be safe to run
//! again, because a crash between the change and its record runs it again at the next boot.
//!
//! The time a change is recorded under is kept, because a change can move what earlier data means:
//! a configuration version published before QR ordering had one switch is read differently from
//! one published since.

use core::future::Future;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};

use pos_proto::time::Timestamp;

/// Why the record of data changes could not be read or written.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("the record of data changes is unavailable: {0}")]
pub struct DataMigrationError(String);

impl DataMigrationError {
    /// An error carrying what went wrong.
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

/// The record of the one-time data changes the cloud has made.
///
/// Implemented over PostgreSQL's `data_migrations` table by `store-postgres`, and in memory by
/// [`InMemoryDataMigrations`].
pub trait DataMigrations {
    /// When the change named `name` was recorded as made, or `None` if it has not been.
    ///
    /// # Errors
    ///
    /// [`DataMigrationError`] if the record cannot be read.
    fn recorded_time(
        &self,
        name: &str,
    ) -> impl Future<Output = Result<Option<Timestamp>, DataMigrationError>> + Send;

    /// Records the change named `name` as made at `at`. Recording a change already recorded changes
    /// nothing, and keeps the time it was first recorded under.
    ///
    /// # Errors
    ///
    /// [`DataMigrationError`] if the record cannot be written.
    fn record(
        &self,
        name: &str,
        at: Timestamp,
    ) -> impl Future<Output = Result<(), DataMigrationError>> + Send;
}

/// The most changes [`InMemoryDataMigrations`] records. There is one name per change ever written
/// into the cloud's code, so this is far more than a deployment will hold.
pub const IN_MEMORY_CHANGES: usize = 256;

/// The record in memory, for tests and for code that runs without a database. Bounded at
/// [`IN_MEMORY_CHANGES`] names.
#[derive(Clone, Debug, Default)]
pub struct InMemoryDataMigrations {
    names: Arc<Mutex<BTreeMap<String, Timestamp>>>,
}

impl InMemoryDataMigrations {
    /// An empty record: no change has been made.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

impl DataMigrations for InMemoryDataMigrations {
    async fn recorded_time(&self, name: &str) -> Result<Option<Timestamp>, DataMigrationError> {
        Ok(self
            .names
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(name)
            .copied())
    }

    async fn record(&self, name: &str, at: Timestamp) -> Result<(), DataMigrationError> {
        let mut names = self.names.lock().unwrap_or_else(PoisonError::into_inner);
        if names.contains_key(name) {
            return Ok(());
        }
        if names.len() >= IN_MEMORY_CHANGES {
            return Err(DataMigrationError::new(format!(
                "the in-memory record holds at most {IN_MEMORY_CHANGES} changes"
            )));
        }
        names.insert(name.to_owned(), at);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use pos_proto::time::Timestamp;

    use super::{DataMigrations, InMemoryDataMigrations};

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(ms).unwrap_or(Timestamp::EPOCH)
    }

    #[tokio::test]
    async fn a_change_reads_as_made_once_recorded_and_recording_it_again_keeps_the_first_time() {
        let record = InMemoryDataMigrations::new();
        assert_eq!(record.recorded_time("one_change").await, Ok(None));
        assert_eq!(record.record("one_change", at(1_000)).await, Ok(()));
        assert_eq!(record.record("one_change", at(2_000)).await, Ok(()));
        assert_eq!(
            record.recorded_time("one_change").await,
            Ok(Some(at(1_000)))
        );
        assert_eq!(record.recorded_time("another_change").await, Ok(None));
    }
}
