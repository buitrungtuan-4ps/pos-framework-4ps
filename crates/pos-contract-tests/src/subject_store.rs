// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `SubjectStore` suite.
//!
//! Two obligations, and the framework's PII posture rests on both
//! ([ADR-0107](../../../docs/adr/0107-the-buyer-is-a-subject.md),
//! [ADR-0035](../../../docs/adr/0035-retention-and-pii-masking.md), `docs/pos-spec.md` §15).
//!
//! **Atomicity.** A subject's details commit with the events that reference them, or not at all. The
//! failing direction is the one with a person in it: a rolled-back settle that still left a buyer's
//! tax code on disk would have the store holding personal data for a sale that never happened, with
//! no event pointing at it and therefore nothing to find it by.
//!
//! **Masking is one-way and idempotent.** It scrubs the values, keeps the keys and the timestamps,
//! and running it twice changes nothing. That is what makes a retention sweep safe to re-run, and it
//! is the property that lets an immutable financial log and a right to erasure coexist: the money is
//! in the events, the person is here, and scrubbing here leaves every figure standing.
//!
//! The cases drive [`Transactional::begin`](pos_ports::Transactional::begin) themselves, because
//! "commits with the caller's transaction" is only observable across a commit boundary.

use std::collections::BTreeMap;

use pos_ports::subject_store::{REDACTION, SubjectRecord, SubjectStore};
use pos_ports::{PortError, PortName, Transactional, TxContext};
use pos_proto::ids::{StoreId, SubjectId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;

use crate::harness::SubjectStoreHarness;
use crate::{CaseFailure, Obligation};

/// Emits every `SubjectStore` case as a `#[test]`.
#[macro_export]
macro_rules! subject_store_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_SUBJECT_STORE,
            module = subject_store,
            cases = [
                starts_holding_nobody,
                a_committed_record_is_found_again,
                an_uncommitted_record_leaves_no_personal_data,
                the_sweep_scrubs_the_values_and_keeps_the_keys,
                the_sweep_is_idempotent,
                the_sweep_leaves_a_record_inside_the_retention_window,
                a_record_is_scoped_to_its_store,
            ]
        }
    };
}

fn atomicity() -> Obligation {
    Obligation::new(
        PortName::SubjectStore,
        "a subject's details commit with the events that reference them, or not at all",
    )
}

fn masking() -> Obligation {
    Obligation::new(
        PortName::SubjectStore,
        "masking scrubs every value, keeps the keys and the stamps, and is idempotent",
    )
}

fn scoping() -> Obligation {
    Obligation::new(
        PortName::SubjectStore,
        "a subject belongs to the store that recorded it",
    )
}

fn subject(seed: u128) -> SubjectId {
    SubjectId::new(Ulid::from_u128(seed))
}

fn at(milliseconds: i64) -> Timestamp {
    Timestamp::from_milliseconds_since_epoch(milliseconds).unwrap_or(Timestamp::EPOCH)
}

/// The buyer on a corporate invoice — the port's first writer, and a realistic set of keys.
fn buyer(collected_at: Timestamp) -> SubjectRecord {
    let mut fields = BTreeMap::new();
    fields.insert("name".to_owned(), "Kabushiki Kaisha Reiwa".to_owned());
    fields.insert("tax_code".to_owned(), "T1234567890123".to_owned());
    fields.insert("address".to_owned(), "1-1 Marunouchi, Chiyoda".to_owned());
    SubjectRecord::new(collected_at, fields)
}

/// Records one subject and commits it, so a case can assert what a *durable* record does.
async fn record_committed<S: SubjectStore>(
    store: &S,
    store_id: StoreId,
    subject_id: SubjectId,
    record: &SubjectRecord,
) -> Result<(), PortError> {
    let mut tx = store.begin().await?;
    store.record(&mut tx, store_id, subject_id, record).await?;
    tx.commit().await
}

/// A fresh store holds nobody, and asking about a stranger is not an error.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn starts_holding_nobody<H: SubjectStoreHarness>(harness: &H) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let found = store.fetch(harness.store_id(), subject(1)).await?;
    atomicity().require(
        found.is_none(),
        "a fresh store reported a subject it never recorded",
    )
}

/// What was committed reads back unchanged — every field, and the collection stamp the retention
/// clock counts from.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_committed_record_is_found_again<H: SubjectStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let record = buyer(at(1_000));
    record_committed(&store, store_id, subject(2), &record).await?;

    let found = store.fetch(store_id, subject(2)).await?;
    let obligation = atomicity();
    let seen = obligation.require_nth(found.as_slice(), 0, "the recorded subject")?;
    obligation.require_eq(seen, &record, "a committed record reads back unchanged")
}

/// The direction with a person in it: a rolled-back transaction leaves no personal data behind.
///
/// If it did, the store would be holding a buyer's name and tax code for a sale that never settled,
/// with no event referencing the subject — so nothing would ever find it, and no retention sweep
/// keyed on a bill would ever come for it.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn an_uncommitted_record_leaves_no_personal_data<H: SubjectStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();

    let mut tx = store.begin().await?;
    store
        .record(&mut tx, store_id, subject(3), &buyer(at(1_000)))
        .await?;
    tx.rollback().await?;

    let found = store.fetch(store_id, subject(3)).await?;
    atomicity().require(
        found.is_none(),
        "a rolled-back transaction left a person's details on disk",
    )
}

/// The sweep replaces every value with the redaction sentinel, keeps the field names, keeps
/// `collected_at`, and stamps `masked_at`.
///
/// The keys survive deliberately: *what kind* of data was held is an audit trail, and losing it
/// would make a store unable to answer what it once processed.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn the_sweep_scrubs_the_values_and_keeps_the_keys<H: SubjectStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let collected_at = at(1_000);
    record_committed(&store, store_id, subject(4), &buyer(collected_at)).await?;

    let obligation = masking();
    let swept = store.mask_before(store_id, at(5_000), at(6_000)).await?;
    obligation.require_eq(&swept, &1, "the sweep reports what it scrubbed")?;

    let found = store.fetch(store_id, subject(4)).await?;
    let seen = obligation.require_nth(found.as_slice(), 0, "the swept subject")?;
    obligation.require_eq(
        &seen.collected_at,
        &collected_at,
        "the collection stamp survives masking",
    )?;
    obligation.require_eq(
        &seen.masked_at,
        &Some(at(6_000)),
        "the record records when it was scrubbed",
    )?;
    obligation.require_eq(
        &seen.fields.keys().cloned().collect::<Vec<_>>(),
        &vec![
            "address".to_owned(),
            "name".to_owned(),
            "tax_code".to_owned(),
        ],
        "the field names survive, so what was held stays knowable",
    )?;
    obligation.require(
        seen.fields.values().all(|value| value == REDACTION),
        "a field survived the sweep with its value intact",
    )
}

/// Running the sweep twice changes nothing, and does not advance the stamp.
///
/// A sweep runs on a timer against a live store; it must be safe to run again after a restart, a
/// clock step, or an interrupted pass.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn the_sweep_is_idempotent<H: SubjectStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    record_committed(&store, store_id, subject(5), &buyer(at(1_000))).await?;

    let obligation = masking();
    store.mask_before(store_id, at(5_000), at(6_000)).await?;
    let again = store.mask_before(store_id, at(9_000), at(9_500)).await?;
    obligation.require_eq(
        &again,
        &0,
        "a second sweep re-scrubbed a record that was already scrubbed",
    )?;

    let found = store.fetch(store_id, subject(5)).await?;
    let seen = obligation.require_nth(found.as_slice(), 0, "the already-swept subject")?;
    obligation.require_eq(
        &seen.masked_at,
        &Some(at(6_000)),
        "the second sweep advanced the stamp, losing when the data actually went",
    )
}

/// A record collected after the cutoff is inside the retention window and is left alone.
///
/// The failure this guards is a sweep that ignores its cutoff and scrubs today's invoices — which
/// would be silent, irreversible, and only discovered when a buyer asked for their copy.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn the_sweep_leaves_a_record_inside_the_retention_window<H: SubjectStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let recent = buyer(at(8_000));
    record_committed(&store, store_id, subject(6), &recent).await?;

    let obligation = masking();
    let swept = store.mask_before(store_id, at(5_000), at(6_000)).await?;
    obligation.require_eq(&swept, &0, "the sweep reached past its own cutoff")?;

    let found = store.fetch(store_id, subject(6)).await?;
    let seen = obligation.require_nth(found.as_slice(), 0, "the still-retained subject")?;
    obligation.require_eq(seen, &recent, "a record inside the window is untouched")
}

/// A subject belongs to the store that recorded it, and another store's sweep does not reach it.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_record_is_scoped_to_its_store<H: SubjectStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let elsewhere = StoreId::new(Ulid::from_u128(0xFFFF_FFFF));
    let record = buyer(at(1_000));
    record_committed(&store, store_id, subject(7), &record).await?;

    let obligation = scoping();
    obligation.require(
        store.fetch(elsewhere, subject(7)).await?.is_none(),
        "another store read a subject it does not hold",
    )?;

    let swept = store.mask_before(elsewhere, at(5_000), at(6_000)).await?;
    obligation.require_eq(
        &swept,
        &0,
        "another store's sweep reached this store's rows",
    )?;

    let found = store.fetch(store_id, subject(7)).await?;
    let seen = obligation.require_nth(found.as_slice(), 0, "the owning store's subject")?;
    obligation.require_eq(seen, &record, "the owning store's record is untouched")
}
