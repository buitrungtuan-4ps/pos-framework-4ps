// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `PostgresPeopleRepublishes` holds the
//! [`PeopleRepublishStore`](pos_cloud::people_republish::PeopleRepublishStore) contract against a
//! real PostgreSQL: the same rules `tests/cloud.rs` holds the in-memory queue to, run through the
//! cloud's own conversion of the adapter's rows.
//!
//! Gated behind the `integration` feature, off by default, with the database the `store-postgres`
//! suite uses:
//!
//! ```text
//! DATABASE_URL="host=localhost port=5432 user=pos password=pos dbname=poscloud" \
//!     cargo test -p pos-cloud --features integration --test people_republish_store_postgres
//! ```
//!
//! The run truncates nothing: every id it writes is fresh, tenant ids included, so it reads only its
//! own rows from a database that keeps an earlier run's, and it clears the rows it queued.

#![cfg(feature = "integration")]

mod people_republish_contract;

use store_postgres::PostgresStore;

#[tokio::test]
async fn the_postgres_queue_holds_the_people_republish_contract() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL names the test database");
    let store = PostgresStore::connect(&url).expect("connect");
    // The schema, idempotently, as a boot applies it.
    store.migrate().await.expect("migrate");
    let mut entropy = [0_u8; 8];
    getrandom::fill(&mut entropy).expect("OS entropy");
    // A fresh high half for this run's ids; the low half is the contract's own small numbers.
    let seed = u128::from(u64::from_be_bytes(entropy)) << 64;
    people_republish_contract::holds(&store.people_republishes(), seed).await;
}
