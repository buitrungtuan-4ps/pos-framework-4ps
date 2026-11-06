// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The store-local subject store: where personal data lives, apart from the log
//! ([ADR-0107](../../../docs/adr/0107-the-buyer-is-a-subject.md)).
//!
//! # The log cannot hold a person, so something else must
//!
//! [`pos_proto::pii`] makes `NoPii` a sealed marker trait and refuses `String`, so a name in an event
//! payload is a compile error. `docs/pos-spec.md` §15 states the consequence: the log is immutable,
//! so personal data inside it could never be erased. What §15 promises instead is a **subject
//! store** — a place beside the log, keyed by [`SubjectId`], where a person's details can be held and
//! then scrubbed while every financial figure stays exactly where it was.
//!
//! The cloud has had one since P7. A **store** has not, because until a till had to print a B2B tax
//! invoice ([ADR-0107](../../../docs/adr/0107-the-buyer-is-a-subject.md)) nothing at a till held
//! personal data. This is that store.
//!
//! # An opaque field map, not a buyer
//!
//! The first writer is a corporate invoice's buyer, and the port is deliberately not shaped like one.
//! A `BuyerStore` would be joined by a second port the first time a delivery contact or a loyalty
//! member needed the same treatment, and personal data would then live in two places — the exact
//! arrangement §15 exists to prevent. So a record is a map of field name to value, and what the keys
//! mean is the caller's business.
//!
//! # There is no read-them-all
//!
//! [`SubjectStore::fetch`] reads **one** subject, by id. A port that could enumerate personal data is
//! a port that can export it, and nothing here has a reason to. The sweep
//! ([`SubjectStore::mask_before`]) is the one bulk operation, and it only ever *destroys* data.

use core::future::Future;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use pos_proto::ids::{StoreId, SubjectId};
use pos_proto::time::Timestamp;

use crate::error::PortError;
use crate::tx::Transactional;

/// What a masked field's value reads as.
///
/// A fixed sentinel rather than an encoding, because masking is **one-way**: the original value is
/// gone, not hidden. The same string the cloud's sweep writes
/// ([ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md)), so an operator reading a store
/// row and a cloud row sees one convention rather than two.
pub const REDACTION: &str = "[REDACTED]";

/// One subject's personal data, as the store holds it.
///
/// The `subject_id` is the key rather than a field, so it cannot disagree with where the record is
/// filed. `collected_at` is when the data was captured and is what the retention clock counts from;
/// masking preserves it, because "held from then until then" is an audit trail rather than personal
/// data.
///
/// A record may carry its own expiry, `retain_until`
/// ([ADR-0206](../../../docs/adr/0206-a-personal-custom-value-stays-on-its-store-and-expires-on-its-own-clock.md)
/// decision 1): a personal custom value is kept for the days its field declares, not for the
/// store's default. A record without one, such as a B2B buyer's (ADR-0107), is swept by the store's
/// default as before.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SubjectRecord {
    /// When the personal data was collected. The retention period counts from here.
    pub collected_at: Timestamp,
    /// The personal fields, by name — `name`, `tax_code`, `address`, whatever the caller captured.
    /// Masking scrubs the values and keeps the keys, so what *kind* of data was held stays knowable.
    pub fields: BTreeMap<String, String>,
    /// When this record was masked, or `None` while it still holds personal data.
    pub masked_at: Option<Timestamp>,
    /// When this record expires, or `None` to keep the store's default retention. Kept by masking,
    /// as `collected_at` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retain_until: Option<Timestamp>,
}

impl SubjectRecord {
    /// A fresh, unmasked record collected at `collected_at`, kept for the store's default retention.
    #[must_use]
    pub fn new(collected_at: Timestamp, fields: BTreeMap<String, String>) -> Self {
        Self {
            collected_at,
            fields,
            masked_at: None,
            retain_until: None,
        }
    }

    /// This record, expiring at `retain_until` rather than at the store's default.
    #[must_use]
    pub fn retained_until(self, retain_until: Timestamp) -> Self {
        Self {
            retain_until: Some(retain_until),
            ..self
        }
    }

    /// Whether the sweep masks this record: unmasked, and either past its own `retain_until` at
    /// `now`, or, without one, collected at or before the store's `cutoff`.
    #[must_use]
    pub fn is_due(&self, cutoff: Timestamp, now: Timestamp) -> bool {
        if self.is_masked() {
            return false;
        }
        match self.retain_until {
            Some(retain_until) => retain_until <= now,
            None => self.collected_at <= cutoff,
        }
    }

    /// Whether the personal data has already been scrubbed.
    #[must_use]
    pub const fn is_masked(&self) -> bool {
        self.masked_at.is_some()
    }

    /// This record with every field value replaced by [`REDACTION`] and `masked_at` stamped at `now`.
    ///
    /// Idempotent: masking an already-masked record yields an equal record, keeping the original
    /// stamp rather than advancing it — so re-running a sweep changes nothing and the audit trail
    /// still says when the data actually went.
    #[must_use]
    pub fn masked(&self, now: Timestamp) -> Self {
        if self.is_masked() {
            return self.clone();
        }
        Self {
            collected_at: self.collected_at,
            fields: self
                .fields
                .keys()
                .map(|key| (key.clone(), REDACTION.to_owned()))
                .collect(),
            masked_at: Some(now),
            retain_until: self.retain_until,
        }
    }
}

/// A store's subject store.
///
/// # Contract
///
/// 1. **`record` buffers into the caller's transaction**, so a subject's details commit atomically
///    with the events that reference the subject — either both land or neither. A settle that
///    committed without its buyer record would print a compliant invoice and keep no evidence of who
///    it was for; a buyer record that committed without its settle would hold a person's tax code for
///    a sale that never happened.
/// 2. **`record` is last-write-wins by id.** Ids are minted fresh per record, so this is not a
///    merge policy anyone relies on — it is the absence of a failure mode. A retried write must not
///    be able to fail a transaction that is otherwise sound.
/// 3. **`fetch` reads one subject**, and reports the record as stored — including its real
///    `masked_at`, so a caller can tell whether anything is left.
/// 4. **`mask_before` is one-way and idempotent.** It scrubs the unmasked records
///    [`SubjectRecord::is_due`] names — past their own `retain_until`, or, without one, collected at
///    or before the cutoff — keeps `subject_id`, `collected_at` and `retain_until`, and returns how
///    many it changed. Running it twice changes nothing the second time.
pub trait SubjectStore: Transactional {
    /// Buffers `record` for `subject_id` into the caller's transaction. Realised at
    /// [`crate::TxContext::commit`].
    ///
    /// # Errors
    ///
    /// [`PortError::internal`] if the record cannot be encoded for storage.
    fn record(
        &self,
        tx: &mut <Self as Transactional>::Tx,
        store_id: StoreId,
        subject_id: SubjectId,
        record: &SubjectRecord,
    ) -> impl Future<Output = Result<(), PortError>> + Send;

    /// One subject's record, or `None` if this store holds none under that id.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be reached, or [`PortError::internal`] if a
    /// stored record cannot be decoded.
    fn fetch(
        &self,
        store_id: StoreId,
        subject_id: SubjectId,
    ) -> impl Future<Output = Result<Option<SubjectRecord>, PortError>> + Send;

    /// Masks every unmasked record whose `retain_until` is at or before `now`, and every unmasked
    /// record without one collected at or before `cutoff`, stamping `now`. Returns how many records
    /// were changed.
    ///
    /// The retention sweep. Not transactional: it destroys data rather than accompanying a write, so
    /// there is nothing for it to be atomic *with*, and a sweep that stops halfway has simply scrubbed
    /// fewer records than it meant to — which the next run finishes.
    ///
    /// # Errors
    ///
    /// [`PortError::unavailable`] if the store cannot be reached, or [`PortError::internal`] if a
    /// stored record cannot be decoded or re-encoded.
    fn mask_before(
        &self,
        store_id: StoreId,
        cutoff: Timestamp,
        now: Timestamp,
    ) -> impl Future<Output = Result<u64, PortError>> + Send;
}

#[cfg(test)]
mod tests {
    use super::{REDACTION, SubjectRecord};

    use std::collections::BTreeMap;

    use pos_proto::time::Timestamp;

    fn at(milliseconds: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(milliseconds).expect("a representable instant")
    }

    fn buyer() -> SubjectRecord {
        let mut fields = BTreeMap::new();
        fields.insert("name".to_owned(), "Kabushiki Kaisha Reiwa".to_owned());
        fields.insert("tax_code".to_owned(), "T1234567890123".to_owned());
        SubjectRecord::new(at(1_000), fields)
    }

    #[test]
    fn a_fresh_record_is_not_masked() {
        assert!(!buyer().is_masked());
    }

    #[test]
    fn masking_scrubs_the_values_and_keeps_the_keys() {
        let masked = buyer().masked(at(2_000));

        assert_eq!(masked.masked_at, Some(at(2_000)));
        assert_eq!(masked.collected_at, at(1_000));
        assert_eq!(
            masked.fields.keys().collect::<Vec<_>>(),
            vec!["name", "tax_code"],
            "what kind of data was held stays knowable"
        );
        assert!(masked.fields.values().all(|value| value == REDACTION));
    }

    #[test]
    fn masking_is_idempotent_and_keeps_the_original_stamp() {
        let once = buyer().masked(at(2_000));
        let twice = once.masked(at(9_000));

        assert_eq!(once, twice, "a second sweep changes nothing");
        assert_eq!(twice.masked_at, Some(at(2_000)));
    }

    #[test]
    fn a_record_with_its_own_expiry_is_due_at_it_whatever_the_cutoff() {
        let kept = buyer().retained_until(at(5_000));

        assert!(!kept.is_due(at(9_000), at(4_999)), "before its expiry");
        assert!(kept.is_due(at(0), at(5_000)), "at its expiry");
        assert_eq!(kept.masked(at(5_000)).retain_until, Some(at(5_000)));
    }

    #[test]
    fn a_record_without_an_expiry_is_due_at_the_cutoff() {
        assert!(buyer().is_due(at(1_000), at(0)));
        assert!(!buyer().is_due(at(999), at(9_000)));
        assert!(!buyer().masked(at(2_000)).is_due(at(9_000), at(9_000)));
    }

    #[test]
    fn a_record_written_before_expiries_reads_back_without_one() {
        let mut json = serde_json::to_value(buyer()).expect("a record encodes");
        assert_eq!(json.get("retain_until"), None, "no expiry, no key");
        json.as_object_mut()
            .map(|fields| fields.remove("retain_until"));
        let record: SubjectRecord = serde_json::from_value(json).expect("an older record decodes");
        assert_eq!(record, buyer());
    }
}
