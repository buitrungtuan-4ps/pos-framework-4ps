// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! The [`PeopleRepublishStore`] contract, held by every implementation of the seam: the in-memory
//! queue in `tests/cloud.rs`, and `PostgresPeopleRepublishes` against a real database in
//! `tests/people_republish_store_postgres.rs` (behind the `integration` feature).
//!
//! The queue holds one row per store. Queuing a store again moves its reason and time forward
//! rather than adding a row; a read hands rows over in tenant then store order, from past the
//! last one the caller read, at most as many as it asked for; and a clear removes a row only while
//! it carries the time it was read with, so a store queued again meanwhile stays queued. Nothing a
//! clear does reaches another store, or the same store id under another tenant.
//!
//! Every read starts past a key just below this run's own tenants, whose ids are fresh, so on a
//! database that keeps rows from other runs it still reads only its own, with exact limits.

use pos_cloud::people_republish::{PeopleRepublish, PeopleRepublishStore};
use pos_proto::ids::{StoreId, TenantId};
use pos_proto::time::Timestamp;
use pos_proto::ulid::Ulid;

/// The ids one run mints, each `seed` plus a small number, so a database that keeps rows from an
/// earlier run never sees one of them twice.
struct Ids(u128);

impl Ids {
    fn tenant(&self, n: u128) -> TenantId {
        TenantId::new(Ulid::from_u128(self.0 + n))
    }
    fn store(&self, n: u128) -> StoreId {
        StoreId::new(Ulid::from_u128(self.0 + 0x100 + n))
    }
    /// The key every read starts past: the first tenant, with a store id below all of this run's.
    fn start(&self) -> (TenantId, StoreId) {
        (self.tenant(1), self.store(0))
    }
}

/// A queue time, to the millisecond, well past the epoch.
fn at(offset_ms: i64) -> Timestamp {
    Timestamp::from_milliseconds_since_epoch(1_760_000_000_000 + offset_ms).expect("a valid time")
}

fn queued(
    tenant_id: TenantId,
    store_id: StoreId,
    reason: &str,
    time: Timestamp,
) -> PeopleRepublish {
    PeopleRepublish {
        tenant_id,
        store_id,
        reason: reason.to_owned(),
        enqueued_time: time,
    }
}

/// The `(tenant, store)` keys of a read, in the order it returned them.
fn keys(rows: &[PeopleRepublish]) -> Vec<(TenantId, StoreId)> {
    rows.iter()
        .map(|row| (row.tenant_id, row.store_id))
        .collect()
}

/// Holds the contract against `store`, minting every id from `seed`.
pub(crate) async fn holds<S: PeopleRepublishStore>(store: &S, seed: u128) {
    let ids = Ids(seed);
    let queued = reads_in_order(store, &ids).await;
    clears_only_what_it_read(store, &ids, &queued).await;
}

/// Queues four stores across two tenants, one of them twice, and reads them back in order, a page
/// at a time. Returns the four rows as they stand.
async fn reads_in_order<S: PeopleRepublishStore>(store: &S, ids: &Ids) -> Vec<PeopleRepublish> {
    let (first, second) = (ids.tenant(1), ids.tenant(2));
    let start = Some(ids.start());

    // Nothing of this run's is queued yet.
    let before = store.pending(start, 10).await.expect("read the queue");
    assert!(
        before
            .iter()
            .all(|row| row.tenant_id != first && row.tenant_id != second),
        "a fresh tenant has nothing queued"
    );

    // Queued out of order, across two tenants, and read back in tenant then store order.
    for (tenant_id, store_id, offset) in [
        (second, ids.store(1), 10),
        (first, ids.store(3), 20),
        (first, ids.store(1), 30),
        (first, ids.store(2), 40),
    ] {
        store
            .enqueue(&queued(
                tenant_id,
                store_id,
                "0083_people_republishes",
                at(offset),
            ))
            .await
            .expect("queue a store");
    }
    let all = store.pending(start, 4).await.expect("read the queue");
    assert_eq!(
        keys(&all),
        vec![
            (first, ids.store(1)),
            (first, ids.store(2)),
            (first, ids.store(3)),
            (second, ids.store(1)),
        ],
        "tenant then store order, whatever order they were queued in"
    );
    assert_eq!(
        all[0],
        queued(first, ids.store(1), "0083_people_republishes", at(30)),
        "a row reads back with its reason and its time to the millisecond"
    );

    // A read takes at most what it was asked for, and the next one starts past the last row read.
    let page = store.pending(start, 2).await.expect("read a page");
    assert_eq!(
        keys(&page),
        vec![(first, ids.store(1)), (first, ids.store(2))]
    );
    let next = store
        .pending(Some((first, ids.store(2))), 2)
        .await
        .expect("read the next page");
    assert_eq!(
        keys(&next),
        vec![(first, ids.store(3)), (second, ids.store(1))],
        "past a key means past it in tenant then store order, into the next tenant"
    );

    // Queued again: still one row, carrying the newer reason and time.
    store
        .enqueue(&queued(first, ids.store(2), "0084_a_later_grant", at(50)))
        .await
        .expect("queue a store again");
    let again = store.pending(start, 4).await.expect("read the queue");
    assert_eq!(again.len(), 4, "queuing a store again adds no row");
    assert_eq!(
        again[1],
        queued(first, ids.store(2), "0084_a_later_grant", at(50)),
        "queuing a store again moves its reason and time forward"
    );
    again
}

/// Clears the rows `read` holds: each while it carries the time it was read with, and never one
/// queued again since, nor another store's.
async fn clears_only_what_it_read<S: PeopleRepublishStore>(
    store: &S,
    ids: &Ids,
    read: &[PeopleRepublish],
) {
    let (first, second) = (ids.tenant(1), ids.tenant(2));
    let start = Some(ids.start());

    // A clear with the time a read returned removes the row, once.
    assert!(
        store.clear(&read[0]).await.expect("clear a row"),
        "a row is cleared while it carries the time it was read with"
    );
    assert!(
        !store.clear(&read[0]).await.expect("clear it again"),
        "a row cleared already is not cleared twice"
    );

    // A clear with the time from before the store was queued again leaves the row as it is.
    let stale = queued(first, ids.store(2), "0083_people_republishes", at(40));
    assert!(
        !store.clear(&stale).await.expect("clear with a stale time"),
        "a store queued again since the read stays queued"
    );
    let left = store.pending(start, 3).await.expect("read the queue");
    assert_eq!(
        left,
        vec![
            queued(first, ids.store(2), "0084_a_later_grant", at(50)),
            queued(first, ids.store(3), "0083_people_republishes", at(20)),
            queued(second, ids.store(1), "0083_people_republishes", at(10)),
        ],
        "only the cleared row is gone: the same store id under another tenant is still queued"
    );

    // Cleared with the times just read, the run leaves nothing behind.
    for row in &left {
        assert!(store.clear(row).await.expect("clear a row"));
    }
    let after = store.pending(start, 10).await.expect("read the queue");
    assert!(
        after
            .iter()
            .all(|row| row.tenant_id != first && row.tenant_id != second),
        "every row this run queued is cleared"
    );
}
