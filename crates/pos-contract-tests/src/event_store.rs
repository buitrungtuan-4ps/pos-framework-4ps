// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The `EventStore` suite.
//!
//! `docs/architecture.md` §5 states this port's contract in one sentence — *"`EventStore` must
//! return events in order, be idempotent by ULID, and survive a simulated crash
//! mid-transaction"* — and this module is that sentence made executable, plus the outbox
//! obligations [ADR-0026](../../../docs/adr/0026-port-shapes.md) §3 adds.
//!
//! Every store the framework ships or will ship runs these cases: `store-sqlite`,
//! `store-postgres`, and the in-memory fake. That last one matters more than it looks — the
//! domain suite runs against the fake, so if the fake and the real store disagree, every domain
//! test is testing the wrong thing.
//!
//! # The chain obligations
//!
//! [ADR-0131](../../../docs/adr/0131-a-chained-event-log.md) decision 5: *"Verification is a
//! contract test, not an adapter's private business."* The four cases at the end of this module are
//! that. They are not conditional on an adapter's goodwill — each harness **declares** whether its
//! store chains ([`chains`](crate::harness::EventStoreHarness::chains)), the declaration is checked
//! against what the store actually produced, and a wrong declaration fails in either direction.

use core::num::NonZeroU32;

use pos_ports::event_store::{EventQuery, EventStore, OutboxPosition};
use pos_ports::{PortName, Transactional, TxContext};
use pos_proto::ErrorStatus;

use crate::harness::EventStoreHarness;
use crate::{CaseFailure, Obligation, fixtures};

/// Emits every `EventStore` case as a `#[test]`.
///
/// ```ignore
/// pos_contract_tests::event_store_suite!(MyHarness::new(), my_block_on);
/// ```
#[macro_export]
macro_rules! event_store_suite {
    ($harness:expr, $block_on:path) => {
        $crate::contract_cases! {
            harness = $harness,
            block_on = $block_on,
            port = $crate::__PORT_EVENT_STORE,
            module = event_store,
            cases = [
                reads_back_in_ascending_order,
                pages_without_gap_or_repeat,
                is_idempotent_by_event_id,
                keeps_the_stored_copy_on_a_collision,
                rolls_back_an_uncommitted_transaction,
                survives_power_loss_mid_transaction,
                keeps_committed_events_across_power_loss,
                reports_an_unknown_store_as_empty,
                drains_the_outbox_in_commit_order,
                acknowledges_as_a_high_water_mark,
                reports_outbox_depth,
                refuses_a_batch_spanning_two_stores,
                starts_at_one_and_links_to_genesis,
                advances_without_gaps,
                reports_the_head_it_stamped,
                a_replay_does_not_advance_the_chain,
                a_partly_replayed_batch_leaves_no_gap,
            ]
        }
    };
}

fn ordering() -> Obligation {
    Obligation::new(PortName::EventStore, "ordered read-back")
}

fn idempotency() -> Obligation {
    Obligation::new(PortName::EventStore, "idempotency by ULID")
}

fn durability() -> Obligation {
    Obligation::new(PortName::EventStore, "survival of a crash mid-transaction")
}

fn outbox() -> Obligation {
    Obligation::new(PortName::EventStore, "outbox monotonicity")
}

/// A page size big enough that no case is accidentally testing pagination.
fn page(size: u32) -> NonZeroU32 {
    NonZeroU32::new(size).unwrap_or(NonZeroU32::MIN)
}

/// Appends `events` in one committed transaction.
async fn append_committed<S: EventStore>(
    store: &S,
    events: &[pos_proto::EventEnvelope<pos_proto::RawPayload>],
) -> Result<pos_ports::AppendOutcome, CaseFailure> {
    let mut tx = store.begin().await?;
    let outcome = store.append(&mut tx, events).await?;
    tx.commit().await?;
    Ok(outcome)
}

/// Events come back sorted by `event_id`, whatever order they went in.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reads_back_in_ascending_order<H: EventStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();

    // Appended out of order on purpose. A store that returns insertion order rather than
    // identifier order passes a naive version of this case and fails here, and the cursor feed
    // in `docs/architecture.md` §6.2 pages by identifier, so insertion order would make a
    // consumer skip events.
    let mut events = fixtures::activations(store_id, 1, 5);
    events.reverse();
    append_committed(&store, &events).await?;

    let read = store.read(&EventQuery::first(store_id, page(100))).await?;
    let obligation = ordering();
    obligation.require_len(&read, 5, "every appended event is read back")?;
    obligation.require_ascending(&read, |event| event.event_id.as_ulid(), "the returned page")?;
    Ok(())
}

/// Paging with `after` yields each event exactly once.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn pages_without_gap_or_repeat<H: EventStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    append_committed(&store, &fixtures::activations(store_id, 1, 7)).await?;

    let obligation = ordering();
    let mut seen = Vec::new();
    let mut cursor = None;
    // Two at a time, so the last page is short and the loop has to notice.
    for _ in 0..10_u32 {
        let mut query = EventQuery::first(store_id, page(2));
        if let Some(after) = cursor {
            query = query.after(after);
        }
        let batch = store.read(&query).await?;
        if batch.is_empty() {
            break;
        }
        obligation.require(
            batch.len() <= 2,
            format!(
                "a page must not exceed the requested limit; got {}",
                batch.len()
            ),
        )?;
        cursor = batch.last().map(|event| event.event_id);
        seen.extend(batch.into_iter().map(|event| event.event_id));
    }

    obligation.require_len(&seen, 7, "paging visits every event")?;
    obligation.require_ascending(&seen, |id| id.as_ulid(), "the concatenated pages")?;
    Ok(())
}

/// Appending a stored `event_id` stores nothing and reports a duplicate.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn is_idempotent_by_event_id<H: EventStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let events = fixtures::activations(store_id, 1, 3);

    let first = append_committed(&store, &events).await?;
    let obligation = idempotency();
    obligation.require_eq(&first.appended, &3, "a first append stores everything")?;
    obligation.require_eq(&first.duplicates, &0, "a first append has no duplicates")?;

    // The same batch again. This is not a hypothetical: at-least-once delivery guarantees it
    // happens, and an implementation that errors here turns a healthy retry into a stuck queue.
    let second = append_committed(&store, &events).await?;
    obligation.require_eq(&second.appended, &0, "a replayed append stores nothing")?;
    obligation.require_eq(
        &second.duplicates,
        &3,
        "a replayed append reports duplicates",
    )?;

    let read = store.read(&EventQuery::first(store_id, page(100))).await?;
    obligation.require_len(&read, 3, "a replay does not grow the log")?;

    // And a mixed batch, one old and one new, because a store that deduplicates whole batches
    // rather than individual events passes both checks above.
    let mixed = vec![
        fixtures::activation(store_id, 3),
        fixtures::activation(store_id, 4),
    ];
    let third = append_committed(&store, &mixed).await?;
    obligation.require_eq(
        &third.appended,
        &1,
        "the new event in a mixed batch is stored",
    )?;
    obligation.require_eq(
        &third.duplicates,
        &1,
        "the old event in a mixed batch is not",
    )?;
    Ok(())
}

/// A collision keeps the stored copy, without comparing bodies.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn keeps_the_stored_copy_on_a_collision<H: EventStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let original = fixtures::activation(store_id, 1);
    let collision = fixtures::activation_with_other_body(store_id, 1);

    append_committed(&store, core::slice::from_ref(&original)).await?;
    let outcome = append_committed(&store, core::slice::from_ref(&collision)).await?;

    let obligation = idempotency();
    obligation.require_eq(
        &outcome.duplicates,
        &1,
        "a colliding identifier is a duplicate whatever the body says",
    )?;

    let read = store.read(&EventQuery::first(store_id, page(10))).await?;
    let stored = obligation.require_nth(&read, 0, "the stored event")?;
    obligation.require_eq(
        stored.data.as_json(),
        original.data.as_json(),
        "the first writer's body survives — a byte difference at the same identifier is a \
         sender bug a store cannot fix, so it must not silently pick the newer one",
    )?;
    Ok(())
}

/// A transaction dropped without committing leaves nothing behind.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn rolls_back_an_uncommitted_transaction<H: EventStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();

    let mut tx = store.begin().await?;
    store
        .append(&mut tx, &fixtures::activations(store_id, 1, 3))
        .await?;
    tx.rollback().await?;

    let read = store.read(&EventQuery::first(store_id, page(10))).await?;
    durability().require_len(&read, 0, "a rolled-back transaction leaves no events")?;
    Ok(())
}

/// Power loss with a transaction open loses exactly that transaction.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn survives_power_loss_mid_transaction<H: EventStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();

    // One committed transaction, then one left open.
    append_committed(&store, &fixtures::activations(store_id, 1, 2)).await?;
    let mut tx = store.begin().await?;
    store
        .append(&mut tx, &fixtures::activations(store_id, 10, 3))
        .await?;
    // `tx` is dropped by `lose_power` taking the store, which is the point: no commit, no
    // rollback, no clean shutdown.
    drop(tx);
    let store = harness.lose_power(store).await?;

    let read = store.read(&EventQuery::first(store_id, page(100))).await?;
    let obligation = durability();
    obligation.require_len(
        &read,
        2,
        "the committed transaction survives and the open one does not",
    )?;
    obligation.require_ascending(&read, |event| event.event_id.as_ulid(), "the surviving log")?;
    for seed in [10_u32, 11, 12] {
        let leaked = read
            .iter()
            .any(|event| event.event_id == fixtures::event_id(seed));
        obligation.require(
            !leaked,
            format!("event {seed} was never committed and must not be in the log"),
        )?;
    }
    Ok(())
}

/// Everything committed before power loss is still there, and still appendable to.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn keeps_committed_events_across_power_loss<H: EventStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    append_committed(&store, &fixtures::activations(store_id, 1, 4)).await?;

    let store = harness.lose_power(store).await?;
    let obligation = durability();

    // Not just readable — writable. A store that reopens read-only, or with a corrupted
    // identifier index, passes a read-only version of this case and then refuses the first sale
    // after a power cut, which is the failure that matters.
    append_committed(&store, &fixtures::activations(store_id, 5, 2)).await?;
    let read = store.read(&EventQuery::first(store_id, page(100))).await?;
    obligation.require_len(&read, 6, "the log is readable and writable after a restart")?;

    // And the identifier index survived, or a replay after a power cut would double-write.
    let replay = append_committed(&store, &fixtures::activations(store_id, 1, 4)).await?;
    obligation.require_eq(
        &replay.duplicates,
        &4,
        "idempotency survives a restart — otherwise the first retry after a power cut \
         duplicates every event it replays",
    )?;
    Ok(())
}

/// A store with no events answers with an empty page rather than an error.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reports_an_unknown_store_as_empty<H: EventStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let obligation = ordering();

    let read = store
        .read(&EventQuery::first(harness.store_id(), page(10)))
        .await?;
    obligation.require_len(&read, 0, "a store with no events reads back empty")?;

    let present = store
        .contains(harness.store_id(), fixtures::event_id(1))
        .await?;
    obligation.require_eq(&present, &false, "an absent event is absent, not an error")?;
    Ok(())
}

/// The outbox hands events out in commit order and only once.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn drains_the_outbox_in_commit_order<H: EventStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let obligation = outbox();

    // Two transactions, and the second one's identifiers sort *below* the first one's. A store
    // ordering the outbox by `event_id` returns them the wrong way round here, which is exactly
    // the lost-write hazard ADR-0026 §3 describes.
    append_committed(&store, &fixtures::activations(store_id, 100, 2)).await?;
    append_committed(&store, &fixtures::activations(store_id, 1, 2)).await?;

    let batch = store
        .outbox_batch(store_id, OutboxPosition::START, page(100))
        .await?;
    obligation.require_len(&batch, 4, "every committed event is in the outbox")?;
    obligation.require_ascending(&batch, |record| record.position, "outbox positions")?;

    let first = obligation.require_nth(&batch, 0, "the first outbox record")?;
    obligation.require_eq(
        &first.envelope.event_id,
        &fixtures::event_id(100),
        "the outbox is ordered by commit, not by identifier — the first transaction's events \
         come first even though their identifiers sort higher",
    )?;
    Ok(())
}

/// Acknowledging is a high-water mark, so repeats and rewinds are harmless.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn acknowledges_as_a_high_water_mark<H: EventStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let obligation = outbox();

    append_committed(&store, &fixtures::activations(store_id, 1, 5)).await?;
    let batch = store
        .outbox_batch(store_id, OutboxPosition::START, page(3))
        .await?;
    obligation.require_len(&batch, 3, "a limited outbox read returns the limit")?;
    let through = obligation
        .require_nth(&batch, 2, "the third outbox record")?
        .position;

    let removed = store.acknowledge_outbox(store_id, through).await?;
    obligation.require_eq(&removed, &3, "acknowledging three records removes three")?;

    let again = store.acknowledge_outbox(store_id, through).await?;
    obligation.require_eq(
        &again,
        &0,
        "a repeated acknowledgement removes nothing — publish-then-acknowledge replays on a \
         crash, so this happens in normal operation",
    )?;

    // Rewinding must not resurrect anything either, or a restarted publisher with a stale
    // cursor would re-send events the cloud already has.
    let rewound = store
        .acknowledge_outbox(store_id, OutboxPosition::START)
        .await?;
    obligation.require_eq(&rewound, &0, "acknowledging backwards removes nothing")?;

    let remaining = store
        .outbox_batch(store_id, OutboxPosition::START, page(100))
        .await?;
    obligation.require_len(
        &remaining,
        2,
        "the unacknowledged remainder is still waiting",
    )?;
    Ok(())
}

/// Depth is what the status bar and the queue-depth alert read.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reports_outbox_depth<H: EventStoreHarness>(harness: &H) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let obligation = outbox();

    obligation.require_eq(
        &store.outbox_depth(store_id).await?,
        &0,
        "a fresh outbox is empty",
    )?;
    append_committed(&store, &fixtures::activations(store_id, 1, 6)).await?;
    obligation.require_eq(
        &store.outbox_depth(store_id).await?,
        &6,
        "depth counts undelivered events — this is the number behind \
         \"Offline — selling normally\" in docs/ui-ux.md §4",
    )?;

    let batch = store
        .outbox_batch(store_id, OutboxPosition::START, page(4))
        .await?;
    let through = obligation
        .require_nth(&batch, 3, "the fourth outbox record")?
        .position;
    store.acknowledge_outbox(store_id, through).await?;
    obligation.require_eq(
        &store.outbox_depth(store_id).await?,
        &2,
        "reading the outbox does not reduce depth; acknowledging does",
    )?;
    Ok(())
}

/// An append that mixes stores is refused rather than silently split.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn refuses_a_batch_spanning_two_stores<H: EventStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let obligation = Obligation::new(PortName::EventStore, "one batch, one store");

    let other_store = pos_proto::StoreId::new(pos_proto::Ulid::from_u128(u128::MAX));
    let mixed = vec![
        fixtures::activation(store_id, 1),
        fixtures::activation(other_store, 2),
    ];

    let mut tx = store.begin().await?;
    let outcome = store.append(&mut tx, &mixed).await;
    obligation.require_error(
        outcome,
        ErrorStatus::InvalidArgument,
        "a batch naming two stores must be refused, not partially applied — in the cloud \
         row-level security would refuse it anyway, and an edge store silently accepting it \
         writes another store's events into this store's log",
    )?;
    Ok(())
}

// --- The chain obligations (ADR-0131 decision 5) -------------------------------------------------

fn chain() -> Obligation {
    Obligation::new(PortName::EventStore, "the hash chain")
}

/// Reads a store's events back in chain order, for the cases below.
async fn chained_records<S: EventStore>(
    store: &S,
    store_id: pos_proto::StoreId,
) -> Result<Vec<pos_proto::EventEnvelope<pos_proto::RawPayload>>, CaseFailure> {
    Ok(store.read(&EventQuery::first(store_id, page(100))).await?)
}

/// Checks the harness's [`chains`](crate::harness::EventStoreHarness::chains) declaration against
/// what the store actually produced, and reports whether the chain cases should go on.
///
/// This is where the declaration stops being a claim and becomes a checked fact. Both directions
/// fail: a store that says it chains and produces no head, and one that says it does not and
/// produces one.
fn declaration_holds<H: EventStoreHarness>(
    harness: &H,
    head: Option<&pos_ports::event_store::ChainAnchor>,
) -> Result<bool, CaseFailure> {
    let obligation = chain();
    match (harness.chains(), head) {
        (true, Some(_)) => Ok(true),
        (false, None) => Ok(false),
        (true, None) => {
            obligation.require(
                false,
                "this harness declares its store chains, but the store reported no head after a \
                 committed append",
            )?;
            Ok(false)
        }
        (false, Some(_)) => {
            obligation.require(
                false,
                "this harness declares its store keeps no chain, but the store reported a head — \
                 the declaration is wrong, and the chain obligations were skipped because of it",
            )?;
            Ok(false)
        }
    }
}

/// The first record a store ever writes sits at `seq` 1 and links to the genesis constant.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn starts_at_one_and_links_to_genesis<H: EventStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    append_committed(&store, &fixtures::activations(store_id, 1, 1)).await?;

    let head = store.chain_head(store_id).await?;
    if !declaration_holds(harness, head.as_ref())? {
        return Ok(());
    }

    let obligation = chain();
    let records = chained_records(&store, store_id).await?;
    let Some(first) = records.first() else {
        obligation.require(false, "the appended record did not read back")?;
        return Ok(());
    };
    let Some(link) = first.chain.as_ref() else {
        obligation.require(
            false,
            "a chaining store must stamp every record it appends; the first carries no link",
        )?;
        return Ok(());
    };
    obligation.require_eq(&link.seq, &1, "a store's first record sits at seq 1")?;
    obligation.require(
        link.prev_hash.is_genesis(),
        "a store's first record chains to the genesis constant — anything else claims a \
         predecessor that never existed",
    )?;
    Ok(())
}

/// A run of appends produces a dense chain: seq 1..n with no gaps, and only the first at genesis.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn advances_without_gaps<H: EventStoreHarness>(harness: &H) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    // Two batches, because a chain that only advances within one transaction is not a chain.
    append_committed(&store, &fixtures::activations(store_id, 1, 3)).await?;
    append_committed(&store, &fixtures::activations(store_id, 4, 2)).await?;

    let head = store.chain_head(store_id).await?;
    if !declaration_holds(harness, head.as_ref())? {
        return Ok(());
    }

    let obligation = chain();
    let records = chained_records(&store, store_id).await?;
    obligation.require_len(&records, 5, "every appended event is read back")?;
    for (index, record) in records.iter().enumerate() {
        let expected = index as u64 + 1;
        let Some(link) = record.chain.as_ref() else {
            obligation.require(false, format!("record {expected} carries no chain link"))?;
            return Ok(());
        };
        obligation.require_eq(
            &link.seq,
            &expected,
            "a store's chain is a dense counter — a gap is a record nobody can account for",
        )?;
        obligation.require_eq(
            &link.prev_hash.is_genesis(),
            &(expected == 1),
            "only the first record chains to genesis; a later one doing so would hide everything \
             before it",
        )?;
    }
    Ok(())
}

/// `chain_head` reports the chain the store actually stamped, not a count of rows.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn reports_the_head_it_stamped<H: EventStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    append_committed(&store, &fixtures::activations(store_id, 1, 4)).await?;

    let head = store.chain_head(store_id).await?;
    if !declaration_holds(harness, head.as_ref())? {
        return Ok(());
    }
    let obligation = chain();
    let Some(anchor) = head else {
        obligation.require(false, "the declaration check admitted a missing head")?;
        return Ok(());
    };
    obligation.require_eq(&anchor.seq, &4, "the head names the length of the chain")?;
    obligation.require(
        !anchor.head.is_genesis(),
        "a store with records does not report genesis as its head",
    )?;
    obligation.require_eq(
        &anchor.head.as_str().len(),
        &64,
        "a chain hash is 64 hexadecimal characters",
    )?;
    obligation.require(
        anchor
            .head
            .as_str()
            .chars()
            .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase()),
        "a chain hash is lowercase hexadecimal, so two stores' heads compare as written",
    )?;
    Ok(())
}

/// A replayed append stores nothing, and must therefore advance nothing.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_replay_does_not_advance_the_chain<H: EventStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    let events = fixtures::activations(store_id, 1, 3);
    append_committed(&store, &events).await?;

    let before = store.chain_head(store_id).await?;
    if !declaration_holds(harness, before.as_ref())? {
        return Ok(());
    }

    // At-least-once delivery guarantees this happens. A chain that advanced on a duplicate would
    // report a length the log does not have, and every later verification would read as broken.
    let replay = append_committed(&store, &events).await?;
    let obligation = chain();
    obligation.require_eq(&replay.appended, &0, "a replayed append stores nothing")?;

    let after = store.chain_head(store_id).await?;
    obligation.require_eq(
        &after.map(|anchor| anchor.seq),
        &before.map(|anchor| anchor.seq),
        "a replay must leave the chain exactly where it was",
    )?;
    Ok(())
}

/// A batch that is half already-stored and half new still produces a dense chain.
///
/// # Why this is its own case
///
/// [`a_replay_does_not_advance_the_chain`] appends a batch that is *entirely* duplicate, and an
/// implementation that advanced its chain on an ignored row can still pass it: nothing is written,
/// so nothing persists. The damage shows only in a **mixed** batch, where a duplicate that advanced
/// the counter pushes the next genuinely new record one position too far and leaves a hole — and
/// every later verification reports a break that never happened. That shape is not a contrivance:
/// it is exactly what reconciliation's re-push produces
/// ([ADR-0040](../../../docs/adr/0040-reconciliation.md)), which offers a whole window of ids and
/// gets back the few the cloud lacks.
///
/// # What this does and does not pin, on today's adapters
///
/// It pins the **observable** outcome — the counts and a dense chain — which is what a port
/// contract should state. It is worth being exact about the rest: `store-sqlite` defends this
/// twice, filtering duplicates in `append` before they ever reach the writer *and* guarding the
/// head advance behind a row actually landing. Removing either one alone still passes, because the
/// other holds. So this case is not a regression test for a particular guard in a particular
/// adapter; it is the obligation an adapter with **no** such defence would fail.
///
/// # Errors
///
/// [`CaseFailure`] if the obligation does not hold.
pub async fn a_partly_replayed_batch_leaves_no_gap<H: EventStoreHarness>(
    harness: &H,
) -> Result<(), CaseFailure> {
    let store = harness.fresh().await?;
    let store_id = harness.store_id();
    append_committed(&store, &fixtures::activations(store_id, 1, 2)).await?;

    let head = store.chain_head(store_id).await?;
    if !declaration_holds(harness, head.as_ref())? {
        return Ok(());
    }

    // Seeds 1 and 2 are already stored; 3 and 4 are new. This is the shape reconciliation's
    // re-push produces, so it is the norm rather than a contrivance.
    let outcome = append_committed(&store, &fixtures::activations(store_id, 1, 4)).await?;
    let obligation = chain();
    obligation.require_eq(&outcome.appended, &2, "only the two new events are stored")?;
    obligation.require_eq(
        &outcome.duplicates,
        &2,
        "the two already held are duplicates",
    )?;

    let records = chained_records(&store, store_id).await?;
    obligation.require_len(&records, 4, "the log holds four events")?;
    for (index, record) in records.iter().enumerate() {
        let expected = index as u64 + 1;
        let Some(link) = record.chain.as_ref() else {
            obligation.require(false, format!("record {expected} carries no chain link"))?;
            return Ok(());
        };
        obligation.require_eq(
            &link.seq,
            &expected,
            "a duplicate that was ignored must not advance the chain, or the next new record \
             leaves a hole nobody can account for",
        )?;
    }

    let after = store.chain_head(store_id).await?;
    obligation.require_eq(
        &after.map(|anchor| anchor.seq),
        &Some(4),
        "the head counts the records the log actually holds",
    )?;
    Ok(())
}
