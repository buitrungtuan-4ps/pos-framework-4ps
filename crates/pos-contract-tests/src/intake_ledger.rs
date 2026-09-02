// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `IntakeLedger` suite.
//!
//! The obligation with the most money attached is [`a_repeat_key_is_refused_at_commit`]. The ledger
//! exists so a marketplace's retry, or the relay's at-least-once redelivery, cannot open a second
//! order and charge a guest twice ([ADR-0064](../../../docs/adr/0064-edge-order-in.md)). That
//! guarantee is not "the row is there" — it is that the row and the `sales.order.opened` events land
//! **in one transaction**, and that a second order on the same key loses at commit rather than
//! duplicating. Both halves are only observable across a commit boundary, which is why these cases
//! drive [`Transactional::begin`](pos_ports::Transactional::begin) themselves rather than being
//! handed a buffered handle.
//!
//! # Why this suite arrived late
//!
//! ADR-0064 called `IntakeLedger` a port when it landed, but it was never given a
//! [`PortName`] variant — so [`crate::SUITES`] had no entry for it, `every_port_has_a_suite` could
//! not see it (that guard iterates `PortName::ALL`), and the port went two slices with no shared
//! suite and no row in `docs/architecture.md` §5. Registering the variant is what put it back under
//! the guard, and this is the suite the guard now demands.

use pos_ports::intake_ledger::{IntakeLedger, IntakeRecord};
use pos_ports::{PortError, PortName, Transactional, TxContext};
use pos_proto::ErrorStatus;
use pos_proto::ids::{OrderId, StoreId};
use pos_proto::money::{CurrencyCode, Money};
use pos_proto::ulid::Ulid;

use crate::harness::IntakeLedgerHarness;
use crate::{CaseFailure, Obligation, fixtures};

/// Emits every `IntakeLedger` case as a `#[test]`.
#[macro_export]
macro_rules! intake_ledger_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_INTAKE_LEDGER,
            module = intake_ledger,
            cases = [
                starts_with_nothing_recorded,
                a_committed_record_is_found_again,
                an_uncommitted_record_is_not_found,
                a_repeat_key_is_refused_at_commit,
                the_same_reference_on_two_channels_is_two_records,
                a_record_round_trips_every_field,
            ]
        }
    };
}

fn atomicity() -> Obligation {
    Obligation::new(
        PortName::IntakeLedger,
        "a record commits with the order's events, or not at all",
    )
}

fn idempotency() -> Obligation {
    Obligation::new(
        PortName::IntakeLedger,
        "a repeat key is refused, never duplicated",
    )
}

fn scoping() -> Obligation {
    Obligation::new(
        PortName::IntakeLedger,
        "the idempotency key is scoped by sales channel",
    )
}

const CHANNEL: &str = "SALES_CHANNEL_TAKEAWAY";
const OTHER_CHANNEL: &str = "SALES_CHANNEL_DELIVERY";
const REFERENCE: &str = "MARKETPLACE-ORDER-1";

fn record(seed: u128) -> IntakeRecord {
    IntakeRecord {
        order_id: OrderId::new(Ulid::from_u128(seed)),
        business_date: fixtures::business_date(),
        total: Money::new(CurrencyCode::VND, 165_000),
        repriced: false,
        awaiting_staff_confirmation: false,
    }
}

/// Records one row and commits it, so a case can assert what a *durable* row does.
async fn record_committed<L: IntakeLedger>(
    ledger: &L,
    store_id: StoreId,
    channel: &str,
    reference: &str,
    row: &IntakeRecord,
) -> Result<(), PortError> {
    let mut tx = ledger.begin().await?;
    ledger
        .record(&mut tx, store_id, channel, reference, row)
        .await?;
    tx.commit().await
}

/// A fresh ledger resolves nothing, and asking is not an error.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn starts_with_nothing_recorded<H: IntakeLedgerHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let ledger = harness.fresh().await?;
    let found = ledger
        .look_up(harness.store_id(), CHANNEL, REFERENCE)
        .await?;
    atomicity().require(
        found.is_none(),
        "a fresh ledger reported a record it never recorded",
    )
}

/// What a committed key produced is what a repeat is owed.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_committed_record_is_found_again<H: IntakeLedgerHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let ledger = harness.fresh().await?;
    let store_id = harness.store_id();
    let row = record(1);
    record_committed(&ledger, store_id, CHANNEL, REFERENCE, &row).await?;

    let found = ledger.look_up(store_id, CHANNEL, REFERENCE).await?;
    let obligation = atomicity();
    let seen = obligation.require_nth(found.as_slice(), 0, "the recorded intake")?;
    obligation.require_eq(seen, &row, "a committed record reads back unchanged")
}

/// The atomicity half from the failing direction: a transaction that is *not* committed leaves no
/// trace.
///
/// This is the case that matters after a crash. If a rolled-back intake still resolved, a retry
/// would be told "already handled" for an order that was never opened — and the guest's food would
/// never be made.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn an_uncommitted_record_is_not_found<H: IntakeLedgerHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let ledger = harness.fresh().await?;
    let store_id = harness.store_id();

    let mut tx = ledger.begin().await?;
    ledger
        .record(&mut tx, store_id, CHANNEL, REFERENCE, &record(2))
        .await?;
    tx.rollback().await?;

    let found = ledger.look_up(store_id, CHANNEL, REFERENCE).await?;
    atomicity().require(
        found.is_none(),
        "a rolled-back record still resolved, so a retry would be wrongly refused",
    )
}

/// The duplicate-order guarantee: a second transaction on a key that already exists fails at commit.
///
/// Refused rather than ignored, and refused *at commit* rather than at `record` — the whole
/// transaction rolls back, so the second order's events do not land either. An insert-or-ignore here
/// would silently open a duplicate order with no ledger row pointing at it.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_repeat_key_is_refused_at_commit<H: IntakeLedgerHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let ledger = harness.fresh().await?;
    let store_id = harness.store_id();
    let first = record(3);
    record_committed(&ledger, store_id, CHANNEL, REFERENCE, &first).await?;

    // A second order arrives on the same key — a retry whose caller did not resolve it first.
    let mut tx = ledger.begin().await?;
    ledger
        .record(&mut tx, store_id, CHANNEL, REFERENCE, &record(4))
        .await?;
    let obligation = idempotency();
    let status = tx.commit().await.err().map(|error| error.status());
    obligation.require_eq(
        &status,
        &Some(ErrorStatus::AlreadyExists),
        "a repeat key fails its commit with AlreadyExists, so the caller knows to resolve it",
    )?;

    // And the first record is the one that stands: the loser of the race changed nothing.
    let found = ledger.look_up(store_id, CHANNEL, REFERENCE).await?;
    let seen =
        obligation.require_nth(found.as_slice(), 0, "the record that had already committed")?;
    obligation.require_eq(
        seen,
        &first,
        "the refused transaction left the committed record untouched",
    )
}

/// One reference on two channels is two orders.
///
/// Two marketplaces numbering their own orders from one is ordinary, so a collision here would
/// refuse a real order.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn the_same_reference_on_two_channels_is_two_records<H: IntakeLedgerHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let ledger = harness.fresh().await?;
    let store_id = harness.store_id();
    let takeaway = record(5);
    let delivery = record(6);
    record_committed(&ledger, store_id, CHANNEL, REFERENCE, &takeaway).await?;
    record_committed(&ledger, store_id, OTHER_CHANNEL, REFERENCE, &delivery).await?;

    let obligation = scoping();
    let on_takeaway = ledger.look_up(store_id, CHANNEL, REFERENCE).await?;
    let on_delivery = ledger.look_up(store_id, OTHER_CHANNEL, REFERENCE).await?;
    obligation.require_eq(
        obligation.require_nth(on_takeaway.as_slice(), 0, "the takeaway record")?,
        &takeaway,
        "the takeaway channel keeps its own record",
    )?;
    obligation.require_eq(
        obligation.require_nth(on_delivery.as_slice(), 0, "the delivery record")?,
        &delivery,
        "the delivery channel keeps its own record under the same reference",
    )
}

/// Every field survives storage.
///
/// `repriced` and `awaiting_staff_confirmation` are the two a retry cannot recompute — they are the
/// answer the caller is owed a second time — and `business_date` is what keys the queue number's
/// reconstruction, so a lost one would renumber a repeat under the wrong trading day.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_record_round_trips_every_field<H: IntakeLedgerHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let ledger = harness.fresh().await?;
    let store_id = harness.store_id();
    let row = IntakeRecord {
        order_id: OrderId::new(Ulid::from_u128(7)),
        business_date: fixtures::business_date(),
        total: Money::new(CurrencyCode::VND, 1),
        repriced: true,
        awaiting_staff_confirmation: true,
    };
    record_committed(&ledger, store_id, CHANNEL, REFERENCE, &row).await?;

    let found = ledger.look_up(store_id, CHANNEL, REFERENCE).await?;
    let obligation = atomicity();
    let seen = obligation.require_nth(found.as_slice(), 0, "the recorded intake")?;
    obligation.require_eq(seen, &row, "every field of a stored record round-trips")
}
