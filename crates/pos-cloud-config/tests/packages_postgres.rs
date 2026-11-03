// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! `PostgresPackages` holds the `PackageStore` contract against a real PostgreSQL, and its tables
//! refuse to change a published version for the role tenant queries run as.
//!
//! Gated behind the `integration` feature, off by default, with the database the `store-postgres`
//! suite uses:
//!
//! ```text
//! DATABASE_URL="host=localhost port=5432 user=pos password=pos dbname=poscloud" \
//!     cargo test -p pos-cloud-config --features integration --test packages_postgres
//! ```

#![cfg(feature = "integration")]

mod package_contract;

use store_postgres::PostgresStore;

#[tokio::test]
async fn postgres_packages_hold_the_package_contract() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL names the test database");
    let store = PostgresStore::connect(&url).expect("connect");
    store.migrate().await.expect("migrate");
    // A fresh high half for this run's ids, so an earlier run's rows are never read.
    let random = pos_cloud_api::console::mint_ulid(1)
        .expect("OS entropy")
        .to_u128();
    let seed = (random & u128::from(u64::MAX)) << 64;
    package_contract::holds(&store.packages(), seed).await;
}
