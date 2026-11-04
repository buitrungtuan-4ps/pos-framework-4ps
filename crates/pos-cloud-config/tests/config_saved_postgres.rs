// Copyright (c) 2026 Pizza 4P's. All rights reserved.
// Proprietary and confidential. Internal use only. See LICENSE.

//! A committed config-tree save tells its hook, and only a committed one
//! ([ADR-0195](../../../docs/adr/0195-the-cloud-wakes-a-store-to-pull-its-config.md)): the cloud wakes
//! a store from this hook, so it must fire after the write is visible to every other connection,
//! and never for a save that lost its race.
//!
//! Gated behind the `integration` feature, off by default:
//!
//! ```text
//! DATABASE_URL="host=localhost port=5432 user=pos password=pos dbname=poscloud" \
//!     cargo test -p pos-cloud-config --features integration --test config_saved_postgres
//! ```

#![cfg(feature = "integration")]

use std::sync::{Arc, Mutex};

use pos_cloud_api::version::UpdateOutcome;
use pos_cloud_config::config_tree::{
    ConfigTreeState, ConfigTreeStore, DEFAULT_K, PublishedVersion,
};
use pos_proto::ids::{ConfigVersionId, StoreId, TenantId};
use pos_proto::ulid::Ulid;
use store_postgres::{ConfigSaved, PostgresStore};

fn state(versions: &[ConfigVersionId]) -> ConfigTreeState {
    ConfigTreeState {
        layers: std::array::from_fn(|_| serde_json::json!({})),
        history: versions
            .iter()
            .map(|id| PublishedVersion {
                id: *id,
                effective: serde_json::json!({}),
                nodes: Vec::new(),
            })
            .collect(),
        k: DEFAULT_K,
    }
}

/// What the hook saw: the store, the version, and the head another connection read at that moment.
type Seen = Arc<Mutex<Vec<(StoreId, ConfigVersionId, Option<ConfigVersionId>)>>>;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_committed_save_tells_its_hook_once_visible_and_a_lost_race_tells_nothing() {
    let url = std::env::var("DATABASE_URL").expect("DATABASE_URL names the test database");
    // A second pool, so "visible" means visible to another connection, not to the writer's own.
    let reader = PostgresStore::connect(&url).expect("connect");
    reader.migrate().await.expect("migrate");
    let seen: Seen = Arc::default();
    let hook = {
        let (seen, reader) = (Arc::clone(&seen), reader.clone());
        ConfigSaved::new(move |tenant, store, version| {
            // Read the row back from the other pool, from inside the hook.
            let head = tokio::task::block_in_place(|| {
                tokio::runtime::Handle::current().block_on(async {
                    reader
                        .config_trees()
                        .load(tenant, store)
                        .await
                        .expect("load")
                        .and_then(|tree| tree.record.history.last().map(|v| v.id))
                })
            });
            seen.lock().expect("seen").push((store, version, head));
        })
    };
    let writer = PostgresStore::connect(&url)
        .expect("connect")
        .with_config_saved(hook);
    let trees = writer.config_trees();

    let random = pos_cloud_api::console::mint_ulid(1)
        .expect("OS entropy")
        .to_u128();
    let base = (random & u128::from(u64::MAX)) << 64;
    let tenant = TenantId::new(Ulid::from_u128(base | 1));
    let store = StoreId::new(Ulid::from_u128(base | 2));
    let v1 = ConfigVersionId::new(Ulid::from_u128(base | 0x0B));
    let v2 = ConfigVersionId::new(Ulid::from_u128(base | 0x0C));

    let created = trees
        .save(tenant, store, &state(&[v1]), None)
        .await
        .expect("save");
    let UpdateOutcome::Updated(at_v1) = created else {
        panic!("the first save creates the row: {created:?}");
    };
    assert_eq!(
        *seen.lock().expect("seen"),
        vec![(store, v1, Some(v1))],
        "told once, after the row is visible to another connection"
    );

    let lost = trees
        .save(tenant, store, &state(&[v1, v2]), None)
        .await
        .expect("save");
    assert_eq!(
        lost,
        UpdateOutcome::VersionMismatch,
        "a second create loses"
    );
    let updated = trees
        .save(tenant, store, &state(&[v1, v2]), Some(&at_v1))
        .await
        .expect("save");
    assert!(matches!(updated, UpdateOutcome::Updated(_)));
    let stale = trees
        .save(tenant, store, &state(&[v1, v2]), Some(&at_v1))
        .await
        .expect("save");
    assert_eq!(stale, UpdateOutcome::VersionMismatch, "a stale write loses");

    assert_eq!(
        *seen.lock().expect("seen"),
        vec![(store, v1, Some(v1)), (store, v2, Some(v2))],
        "the two lost races told nothing"
    );
}
