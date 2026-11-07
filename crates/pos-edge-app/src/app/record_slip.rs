// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A custom record's slip: the paper handed to the person a record is about
//! ([ADR-0207](../../../../docs/adr/0207-a-receipt-is-a-structured-template-per-store-and-channel.md)
//! decisions 4 and 5, [ADR-0206](../../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)
//! decision 9).
//!
//! The slip is the person's own copy where the template marks it so. It prints a personal value as
//! the act that saved the record does: the first slip the person who saved it asks for, at the
//! device they saved it at, before they save another record. Any other slip is a reprint, and
//! prints a personal value only for someone holding `data.personal.read`, each subject printed
//! audited.
//!
//! Which records were just saved is kept in memory only, at most [`FRESH_SLIPS_MAX`] people's. A
//! restart forgets them, and every slip after it is a reprint: forgetting withholds, never shows.

use std::collections::VecDeque;

use pos_core::decision::Actor;
use pos_core::permission::Permission;
use pos_ports::event_store::EventStore;
use pos_ports::subject_store::SubjectStore;
use pos_proto::ids::CustomRecordId;
use pos_proto::text::TranslationKey;

use crate::app::{AppError, Edge, PersonalOnPaper};

/// The most people whose last saved record the edge remembers for its first slip.
pub const FRESH_SLIPS_MAX: usize = 64;

/// The record each person last saved at a device whose slip has not been printed yet.
#[derive(Debug, Default)]
pub(crate) struct FreshSlips {
    saved: VecDeque<(Actor, CustomRecordId)>,
}

impl FreshSlips {
    /// `actor` has just saved `custom_record_id`: its next slip by them is the act's own. Anyone
    /// else's first slip of that record becomes a reprint, since it no longer shows what they
    /// saved.
    pub(crate) fn saved(&mut self, actor: Actor, custom_record_id: CustomRecordId) {
        self.saved
            .retain(|(who, record)| *who != actor && *record != custom_record_id);
        if self.saved.len() >= FRESH_SLIPS_MAX {
            self.saved.pop_front();
        }
        self.saved.push_back((actor, custom_record_id));
    }

    /// Whether `actor`'s slip of `custom_record_id` is the first since they saved it; it is then
    /// no longer.
    fn take(&mut self, actor: Actor, custom_record_id: CustomRecordId) -> bool {
        let before = self.saved.len();
        self.saved
            .retain(|entry| *entry != (actor, custom_record_id));
        self.saved.len() != before
    }
}

/// What a record's slip prints under, decided for the person asking.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecordSlipCopy {
    /// The record's object type's label key, the slip's title, where the running package declares
    /// the type.
    pub object_label_key: Option<TranslationKey>,
    /// How a personal value may print on it, if the template marks the slip as the person's copy.
    pub personal: PersonalOnPaper,
}

impl<S: EventStore + SubjectStore> Edge<S> {
    /// The slip `actor` asks to print of `custom_record_id`: the act's own the first time after
    /// they saved it, otherwise a reprint by them. Needs `data.record.write`, the permission the
    /// record is entered under.
    ///
    /// # Errors
    ///
    /// [`AppError::Domain`] without `data.record.write`, or [`AppError::UnknownCustomRecord`] for a
    /// record this edge does not hold.
    pub async fn record_slip(
        &self,
        actor: Actor,
        custom_record_id: CustomRecordId,
    ) -> Result<RecordSlipCopy, AppError> {
        self.decision_ctx(actor)?
            .require(Permission::WriteDataRecord)?;
        let record = self
            .custom_record(custom_record_id)
            .ok_or(AppError::UnknownCustomRecord)?;
        let fresh = self
            .custom_writes
            .lock()
            .await
            .take(actor, custom_record_id);
        let object_label_key = self.session().package.as_ref().and_then(|package| {
            package
                .data
                .object_types
                .iter()
                .find(|kind| Some(kind.object_type_id) == record.object_type_id)
                .map(|kind| kind.label_key.clone())
        });
        Ok(RecordSlipCopy {
            object_label_key,
            personal: if fresh {
                PersonalOnPaper::Shown
            } else {
                PersonalOnPaper::Reprinted(actor)
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use pos_core::decision::Actor;
    use pos_proto::ids::{CustomRecordId, DeviceId, EmployeeId};
    use pos_proto::ulid::Ulid;

    use super::{FRESH_SLIPS_MAX, FreshSlips};

    fn actor(n: u128) -> Actor {
        Actor {
            employee_id: EmployeeId::new(Ulid::from_u128(n)),
            device_id: DeviceId::new(Ulid::from_u128(1)),
        }
    }

    fn record(n: u128) -> CustomRecordId {
        CustomRecordId::new(Ulid::from_u128(n))
    }

    #[test]
    fn the_first_slip_after_a_save_is_the_savers_once() {
        let mut fresh = FreshSlips::default();
        fresh.saved(actor(1), record(1));
        assert!(
            !fresh.take(actor(2), record(1)),
            "someone else's is a reprint"
        );
        assert!(fresh.take(actor(1), record(1)));
        assert!(!fresh.take(actor(1), record(1)), "the second is a reprint");
    }

    #[test]
    fn a_later_save_ends_an_earlier_ones_first_slip() {
        let mut fresh = FreshSlips::default();
        fresh.saved(actor(1), record(1));
        fresh.saved(actor(1), record(2));
        assert!(
            !fresh.take(actor(1), record(1)),
            "another record saved since"
        );
        fresh.saved(actor(2), record(2));
        assert!(
            !fresh.take(actor(1), record(2)),
            "changed by someone else since"
        );
        assert!(fresh.take(actor(2), record(2)));
    }

    #[test]
    fn it_remembers_a_bounded_number_of_people() {
        let mut fresh = FreshSlips::default();
        for n in 0..=FRESH_SLIPS_MAX as u128 {
            fresh.saved(actor(n), record(n));
        }
        assert!(!fresh.take(actor(0), record(0)), "the oldest is forgotten");
        assert!(fresh.take(
            actor(FRESH_SLIPS_MAX as u128),
            record(FRESH_SLIPS_MAX as u128)
        ));
    }
}
