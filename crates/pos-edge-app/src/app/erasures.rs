// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! Masking a subject the cloud asked this store to erase
//! ([ADR-0206](../../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)
//! decision 4).
//!
//! A personal custom value stays on the store that collected it, so an erasure reaches the store
//! by subject id, on its `erasures` config node. For each id the store masks its record with the
//! same one-way `[REDACTED]` masking its own expiry uses, and confirms with `data.subject.masked`,
//! the record and the event in one transaction. An id it has already confirmed is skipped, so a
//! node applied twice changes nothing and confirms nothing twice.

use pos_core::business_date::derive_business_date;
use pos_ports::TxContext;
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;
use pos_proto::ClockSource;
use pos_proto::events::DataSubjectMasked;
use pos_proto::ids::{DeviceId, SubjectId};

use crate::app::{AppError, Edge};

impl<S: EventStore + SubjectStore> Edge<S> {
    /// Masks each subject in `subject_ids` that this store has not yet confirmed, and returns how
    /// many it confirmed. `device_id` is the box's own: no employee is behind an erasure.
    ///
    /// A record not yet masked is masked now. One masked already, by its own expiry, keeps its
    /// stamp, and the event says when that was. A subject this store holds no record of is
    /// confirmed too, at the time of the erasure: the store holds nothing personal for it. Every
    /// id is attempted before an error is returned.
    ///
    /// Serialised with custom-data writes, so a write cannot put back a value an erasure has just
    /// masked. No value is logged: only the subject id and the count.
    ///
    /// # Errors
    ///
    /// [`AppError`] if the business date cannot be derived, or a record cannot be read or written.
    pub async fn erase_subjects(
        &self,
        device_id: DeviceId,
        subject_ids: &[SubjectId],
    ) -> Result<usize, AppError> {
        let _serialised = self.custom_writes.lock().await;
        let mut confirmed = 0;
        let mut failure = None;
        for &subject_id in subject_ids {
            if self.lock_projection().confirmed_masked(subject_id) {
                continue;
            }
            match self.erase_subject(device_id, subject_id).await {
                Ok(()) => {
                    confirmed += 1;
                    tracing::info!(
                        target: "audit",
                        action = "data.subject.masked",
                        subject_id = %subject_id,
                        "a subject was masked on the cloud's instruction"
                    );
                }
                Err(error) => failure = Some(error),
            }
        }
        failure.map_or(Ok(confirmed), Err)
    }

    /// Masks one subject and confirms it, in one transaction.
    async fn erase_subject(
        &self,
        device_id: DeviceId,
        subject_id: SubjectId,
    ) -> Result<(), AppError> {
        let store_id = self.identity.store_id;
        let now = self.clock.now();
        let session = self.session();
        let business_date = derive_business_date(now, &session.timezone, session.cutoff)
            .map_err(|_ignored| AppError::Clock)?;
        let held = self.store.fetch(store_id, subject_id).await?;
        let masked = held.as_ref().map(|record| record.masked(now));
        let event = DataSubjectMasked {
            subject_id,
            masked_time: masked
                .as_ref()
                .and_then(|record| record.masked_at)
                .unwrap_or(now),
        };
        let (envelope, message) = self.system_prepare(device_id, now, business_date, &event)?;
        let mut tx = self.store.begin().await?;
        self.store.append(&mut tx, &[envelope]).await?;
        if let (Some(before), Some(after)) = (&held, &masked)
            && !before.is_masked()
        {
            self.store
                .record(&mut tx, store_id, subject_id, after)
                .await?;
        }
        tx.commit().await?;
        self.fanout.publish(&message);
        self.lock_projection().fold_subject_masked(&event);
        Ok(())
    }
}

#[cfg(test)]
mod tests;
