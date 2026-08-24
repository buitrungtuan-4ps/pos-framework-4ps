// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The personal-data record the retention cron masks, and the masking itself
//! ([ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md)).
//!
//! Personal data never enters the event log ([`pos_proto::pii`]): events carry a
//! [`SubjectId`] and the person's details — a marketplace order's
//! name/phone/address, a corporate invoice's buyer fields — live in a separate **subject store**
//! keyed by that id. So "anonymise a person" is "mask one row here", and because the money figures
//! are all in the events, the books still reconcile after masking.
//!
//! Masking replaces every personal field's *value* with [`REDACTION`] and stamps `masked_at`, while
//! keeping the `subject_id` and `collected_at`. That is deliberate: the id must survive so an invoice
//! still references a subject (reconciliation), and `collected_at`/`masked_at` are an audit trail of
//! what was held and when it was scrubbed — neither is personal data. Masking is one-way and
//! idempotent: masking an already-masked record changes nothing.

use std::collections::BTreeMap;

use pos_proto::ids::SubjectId;
use pos_proto::time::Timestamp;

/// What a masked personal field reads as. Matches the organisation's redaction convention, and is a
/// fixed sentinel so masking is irreversible — the original value is gone, not encoded.
pub const REDACTION: &str = "[REDACTED]";

/// One person's stored personal data — the subject store's row.
///
/// The `fields` are the personal data proper (keys such as `name`, `phone`, `address`, `email`,
/// `tax_code`); they are held here and *never* in an event. `collected_at` is when the data was
/// captured, which is what the retention clock runs from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubjectRecord {
    /// The subject this record is about — the id events reference.
    pub subject_id: SubjectId,
    /// When the personal data was collected; the retention period counts from here.
    pub collected_at: Timestamp,
    /// The personal fields, by name. Masking scrubs the values, not the keys.
    pub fields: BTreeMap<String, String>,
    /// When this record was masked, or `None` while it still holds personal data.
    pub masked_at: Option<Timestamp>,
}

impl SubjectRecord {
    /// Whether the record's personal data has already been masked.
    #[must_use]
    pub fn is_masked(&self) -> bool {
        self.masked_at.is_some()
    }

    /// Returns this record with every personal field value replaced by [`REDACTION`] and `masked_at`
    /// stamped at `now`. The `subject_id` and `collected_at` are preserved.
    ///
    /// Idempotent: masking an already-masked record yields an equal record (the stamp is kept, not
    /// advanced), so re-running the sweep is safe.
    #[must_use]
    pub fn masked(&self, now: Timestamp) -> Self {
        if self.is_masked() {
            return self.clone();
        }
        let fields = self
            .fields
            .keys()
            .map(|key| (key.clone(), REDACTION.to_owned()))
            .collect();
        Self {
            subject_id: self.subject_id,
            collected_at: self.collected_at,
            fields,
            masked_at: Some(now),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{REDACTION, SubjectRecord};

    use std::collections::BTreeMap;

    use pos_proto::ids::SubjectId;
    use pos_proto::time::Timestamp;
    use pos_proto::ulid::Ulid;

    fn at(ms: i64) -> Timestamp {
        Timestamp::from_milliseconds_since_epoch(ms).expect("valid instant")
    }

    /// A record with synthetic placeholder values — never data resembling a real person.
    fn a_record() -> SubjectRecord {
        let mut fields = BTreeMap::new();
        fields.insert("name".to_owned(), "name-placeholder".to_owned());
        fields.insert("phone".to_owned(), "phone-placeholder".to_owned());
        fields.insert("address".to_owned(), "address-placeholder".to_owned());
        SubjectRecord {
            subject_id: SubjectId::new(Ulid::from_u128(0xBEEF)),
            collected_at: at(1_000),
            fields,
            masked_at: None,
        }
    }

    #[test]
    fn masking_scrubs_every_value_but_keeps_the_keys_id_and_timestamps() {
        let masked = a_record().masked(at(9_000));
        assert!(masked.is_masked());
        assert_eq!(masked.masked_at, Some(at(9_000)));
        // The id and collection time survive, so an invoice still references a subject and the audit
        // trail is intact.
        assert_eq!(masked.subject_id, a_record().subject_id);
        assert_eq!(masked.collected_at, a_record().collected_at);
        // Every value is gone; the keys remain so it is clear what categories were held.
        assert_eq!(masked.fields.len(), 3);
        for value in masked.fields.values() {
            assert_eq!(value, REDACTION);
        }
    }

    #[test]
    fn masking_is_idempotent() {
        let once = a_record().masked(at(9_000));
        // A second pass at a later time must not advance the stamp or change anything.
        let twice = once.masked(at(50_000));
        assert_eq!(once, twice, "re-masking a masked record is a no-op");
    }

    #[test]
    fn no_original_value_survives_masking() {
        let original = a_record();
        let masked = original.masked(at(9_000));
        let rendered = format!("{masked:?}");
        for value in original.fields.values() {
            assert!(
                !rendered.contains(value),
                "an original value leaked through masking: {value}"
            );
        }
    }
}
