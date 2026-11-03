// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A transaction dropped open never goes back to the pool, so the pool need not end one on every
//! checkout — the `ROLLBACK` that drew PostgreSQL's "there is no transaction in progress" warning on
//! each of them.

use super::{block_on, prepared};
use pos_ports::{EventStore, Transactional};
use pos_proto::{StoreId, Ulid};

/// Dropping a transaction without committing it — a crash, a cancelled request — closes its
/// connection, so no backend is left idle in that transaction for the next caller to run inside.
#[test]
fn a_transaction_dropped_open_closes_its_connection_rather_than_pooling_it() {
    block_on(async {
        let (store, admin) = prepared().await.expect("prepare the database");
        let store_id = StoreId::new(Ulid::from_u128(0xD20F));
        let event = pos_contract_tests::fixtures::activation(store_id, 1);
        let mut tx = store.begin().await.expect("begin");
        store
            .append(&mut tx, core::slice::from_ref(&event))
            .await
            .expect("append");
        drop(tx);

        let mut open = i64::MAX;
        // The close reaches the server a moment after the drop; a pooled connection never would.
        for _ in 0..50 {
            open = admin
                .query_one(
                    "SELECT count(*) FROM pg_stat_activity \
                     WHERE datname = current_database() AND pid <> pg_backend_pid() \
                       AND state LIKE 'idle in transaction%'",
                    &[],
                )
                .await
                .expect("read the server's sessions")
                .get(0);
            if open == 0 {
                break;
            }
            // The server waits, so this task never blocks its runtime.
            admin
                .batch_execute("SELECT pg_sleep(0.1)")
                .await
                .expect("wait");
        }
        assert_eq!(
            open, 0,
            "no backend is left idle in the dropped transaction"
        );

        // And the store goes on working on a fresh connection, without the dropped event.
        assert!(
            !store
                .contains(store_id, event.event_id)
                .await
                .expect("read"),
            "the dropped transaction's event was never committed"
        );
    });
}
